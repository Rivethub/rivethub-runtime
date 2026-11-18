use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use croner::Cron;
use croner::parser::{CronParser, Seconds};
use tokio_util::sync::CancellationToken;
use wire::plugin::host::{PluginJobOutcome, PluginJobRun, PluginJobTrigger, PluginLogStream};
use wire::plugin::manifest::{PluginJob, PluginManifest};
use wire::plugin::process::{PluginJobRequest, PluginJobResult};
use wire::plugin::{PluginId, RouteName};

use super::{HostError, PluginHost};

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(600_000);

fn parser() -> CronParser {
    CronParser::builder().seconds(Seconds::Disallowed).build()
}

pub(super) fn check_schedules(manifest: &PluginManifest) -> Result<(), HostError> {
    for (index, job) in manifest.jobs.iter().enumerate() {
        if let Some(schedule) = &job.schedule {
            parser()
                .parse(schedule)
                .map_err(|error| HostError::Invalid(format!("jobs[{index}].schedule: {error}")))?;
        }
    }
    Ok(())
}

struct Scheduled {
    job: RouteName,
    cron: Cron,
}

pub(super) async fn schedule(
    host: Arc<PluginHost>,
    plugin: PluginId,
    jobs: Vec<PluginJob>,
    stop: CancellationToken,
) {
    let scheduled: Vec<Scheduled> = jobs
        .into_iter()
        .filter_map(|job| {
            let schedule = job.schedule?;
            match parser().parse(&schedule) {
                Ok(cron) => Some(Scheduled { job: job.id, cron }),
                Err(error) => {
                    tracing::warn!(%error, job = job.id.as_ref(), "plug-in job schedule unreadable");
                    None
                }
            }
        })
        .collect();
    if scheduled.is_empty() {
        return;
    }
    loop {
        let now = Local::now();
        let next: Vec<(DateTime<Local>, &RouteName)> = scheduled
            .iter()
            .filter_map(|entry| match entry.cron.find_next_occurrence(&now, false) {
                Ok(at) => Some((at, &entry.job)),
                Err(error) => {
                    tracing::warn!(%error, job = entry.job.as_ref(), "plug-in job has no next run");
                    None
                }
            })
            .collect();
        let Some(soonest) = next.iter().map(|(at, _)| *at).min() else {
            return;
        };
        let wait = match (soonest - now).to_std() {
            Ok(wait) => wait,
            Err(error) => {
                tracing::trace!(%error, "the next plug-in job run is already due");
                Duration::ZERO
            }
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        for (at, job) in next.iter().filter(|(at, _)| *at == soonest) {
            match start(
                &host,
                &plugin,
                job,
                PluginJobTrigger::Schedule,
                Some(at.with_timezone(&Utc)),
            ) {
                Ok(_) => {}
                Err(HostError::Conflict(reason)) => host.log(
                    &plugin,
                    PluginLogStream::Host,
                    format!("job {} skipped: {reason}", job.as_ref()),
                ),
                Err(error) => host.log(
                    &plugin,
                    PluginLogStream::Host,
                    format!("job {} not started: {error}", job.as_ref()),
                ),
            }
        }
    }
}

pub(super) fn start(
    host: &Arc<PluginHost>,
    plugin: &PluginId,
    job: &RouteName,
    trigger: PluginJobTrigger,
    scheduled_at: Option<DateTime<Utc>>,
) -> Result<PluginJobRun, HostError> {
    let declared = host.job(plugin, job)?;
    let run = PluginJobRun {
        run_id: uuid::Uuid::new_v4().to_string(),
        trigger,
        started_at: Utc::now(),
        finished_at: None,
        outcome: None,
        message: None,
    };
    host.begin_job(plugin, job, run.clone())?;
    let request = PluginJobRequest {
        run_id: run.run_id.clone(),
        trigger,
        scheduled_at,
    };
    let timeout = declared
        .timeout_ms
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
    let host_for_run = Arc::clone(host);
    let plugin = plugin.clone();
    let job = job.clone();
    let started = run.clone();
    host.tasks.spawn(async move {
        let result = execute(&host_for_run, &plugin, &job, &request, timeout).await;
        let (outcome, message) = match result {
            Ok(answer) => (answer.outcome, answer.message),
            Err(detail) => (PluginJobOutcome::Failed, Some(detail)),
        };
        host_for_run.finish_job(
            &plugin,
            &job,
            PluginJobRun {
                finished_at: Some(Utc::now()),
                outcome: Some(outcome),
                message,
                ..started
            },
        );
    });
    Ok(run)
}

async fn execute(
    host: &PluginHost,
    plugin: &PluginId,
    job: &RouteName,
    request: &PluginJobRequest,
    timeout: Duration,
) -> Result<PluginJobResult, String> {
    let client = host
        .running_client(plugin)
        .ok_or_else(|| "the plug-in is not running".to_owned())?;
    let response = client
        .post(super::plugin_url(&format!("jobs/{}", job.as_ref())))
        .timeout(timeout)
        .json(request)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "timed out".to_owned()
            } else {
                error.to_string()
            }
        })?;
    if !response.status().is_success() {
        return Err(format!("answered {}", response.status()));
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    super::parse::<PluginJobResult>(&bytes)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::parser;

    #[test]
    fn plugin_schedules_use_five_fields_and_find_the_next_local_occurrence()
    -> Result<(), Box<dyn std::error::Error>> {
        for (schedule, now, next) in [
            (
                "*/15 * * * *",
                "2026-12-31T23:45:00-05:00",
                "2027-01-01T00:00:00-05:00",
            ),
            (
                "30 8 * * MON",
                "2026-11-15T12:00:00-05:00",
                "2026-11-16T08:30:00-05:00",
            ),
            (
                "@daily",
                "2026-01-01T00:00:00+05:30",
                "2026-01-02T00:00:00+05:30",
            ),
        ] {
            let start = DateTime::parse_from_rfc3339(now)?;
            let next_at = parser()
                .parse(schedule)?
                .find_next_occurrence(&start, false)?;
            assert_eq!(next_at.to_rfc3339(), next);
        }
        assert!(parser().parse("0 0 0 * * *").is_err());
        Ok(())
    }
}

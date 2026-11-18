use jiff::tz::AmbiguousOffset;
use jiff::{SignedDuration, Timestamp, ZonedDateTime};
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;

use croner::Cron;
use croner::errors::CronError;
use croner::parser::{CronParser, Seconds};
use croner::time::{CivilDate, CivilDateTime, CivilTime, CronDateTime, Resolution, Weekday};
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

#[derive(Clone)]
struct ScheduleTime(ZonedDateTime);

impl CronDateTime for ScheduleTime {
    fn to_civil(&self) -> CivilDateTime {
        let date = self.0.datetime();
        CivilDateTime::new(
            CivilDate::from_parts_unchecked(
                i32::from(date.year()),
                u32::from(date.month().unsigned_abs()),
                u32::from(date.day().unsigned_abs()),
            ),
            CivilTime::from_parts_unchecked(
                u32::from(date.hour().unsigned_abs()),
                u32::from(date.minute().unsigned_abs()),
                u32::from(date.second().unsigned_abs()),
            ),
        )
    }

    fn civil_weekday(&self) -> Weekday {
        Weekday::from_days_from_sunday(u32::from(
            self.0.weekday().to_sunday_zero_offset().unsigned_abs(),
        ))
    }

    fn resolve_civil(&self, civil: CivilDateTime) -> Result<Resolution<Self>, CronError> {
        let year = i16::try_from(civil.year()).map_err(|_error| CronError::InvalidDate)?;
        let field = |value| i8::try_from(value).map_err(|_error| CronError::InvalidTime);
        let wall = jiff::civil::DateTime::new(
            year,
            field(civil.month())?,
            field(civil.day())?,
            field(civil.hour())?,
            field(civil.minute())?,
            field(civil.second())?,
            0,
        )
        .map_err(|_error| CronError::InvalidDate)?;
        let zone = self.0.time_zone();
        let possible = zone.to_ambiguous_timestamp(wall);
        let zoned = |at: Result<Timestamp, jiff::Error>| {
            at.map(|at| Self(at.to_zoned_datetime(zone.clone())))
                .map_err(|_error| CronError::InvalidTime)
        };
        match possible.offset() {
            AmbiguousOffset::Unambiguous { .. } => {
                zoned(possible.unambiguous()).map(Resolution::Single)
            }
            AmbiguousOffset::Fold { .. } => Ok(Resolution::Ambiguous(
                zoned(possible.earlier())?,
                zoned(possible.later())?,
            )),
            AmbiguousOffset::Gap { .. } => Ok(Resolution::Gap),
        }
    }

    fn checked_add_seconds(&self, seconds: i64) -> Option<Self> {
        match self.0.checked_add(SignedDuration::from_secs(seconds)) {
            Ok(at) => Some(Self(at)),
            Err(error) => {
                tracing::trace!(%error, "plug-in schedule exceeds the supported timestamp range");
                None
            }
        }
    }

    fn cmp_instant(&self, other: &Self) -> Ordering {
        self.0.timestamp().cmp(&other.0.timestamp())
    }
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
        let now = ScheduleTime(ZonedDateTime::now());
        let next: Vec<(Timestamp, &RouteName)> = scheduled
            .iter()
            .filter_map(|entry| match entry.cron.find_next_occurrence(&now, false) {
                Ok(at) => Some((at.0.timestamp(), &entry.job)),
                Err(error) => {
                    tracing::warn!(%error, job = entry.job.as_ref(), "plug-in job has no next run");
                    None
                }
            })
            .collect();
        let Some(soonest) = next.iter().map(|(at, _)| at).min() else {
            return;
        };
        let wait = match Duration::try_from(soonest.duration_since(now.0.timestamp())) {
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
        for (at, job) in next.iter().filter(|(at, _)| at == soonest) {
            match start(&host, &plugin, job, PluginJobTrigger::Schedule, Some(*at)) {
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
    scheduled_at: Option<Timestamp>,
) -> Result<PluginJobRun, HostError> {
    let declared = host.job(plugin, job)?;
    let run = PluginJobRun {
        run_id: uuid::Uuid::new_v4().to_string(),
        trigger,
        started_at: Timestamp::now(),
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
                finished_at: Some(Timestamp::now()),
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

    use super::{ScheduleTime, parser};
    use croner::errors::CronError;
    use jiff::ZonedDateTime;

    #[test]
    fn numeric_sunday_with_last_modifier_reaches_the_search_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let start = ScheduleTime("2024-09-30T20:00:00-04:00[America/New_York]".parse()?);
        let result = parser()
            .parse("0 0 * * 7L")?
            .find_next_occurrence(&start, false);
        assert!(matches!(result, Err(CronError::TimeSearchLimitExceeded)));
        Ok(())
    }

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
            let offset = now.get(19..).ok_or("offset")?;
            let start = ScheduleTime(format!("{now}[{offset}]").parse::<ZonedDateTime>()?);
            let next_at = parser()
                .parse(schedule)?
                .find_next_occurrence(&start, false)?;
            assert_eq!(next_at.0.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(), next);
        }
        assert!(parser().parse("0 0 0 * * *").is_err());
        Ok(())
    }

    #[test]
    fn plugin_schedules_keep_calendar_rules_and_dst() -> Result<(), Box<dyn std::error::Error>> {
        for (schedule, now, next) in [
            (
                "5,20,45 8-10 * JAN,MAR MON-FRI",
                "2026-01-05T09:05:00-05:00",
                "2026-01-05T09:20:00-05:00",
            ),
            (
                "10-50/15 10 * * *",
                "2026-01-05T10:10:00-05:00",
                "2026-01-05T10:25:00-05:00",
            ),
            (
                "0 9 L * *",
                "2026-02-01T07:00:00-05:00",
                "2026-02-28T09:00:00-05:00",
            ),
            (
                "0 9 15W * *",
                "2026-02-01T07:00:00-05:00",
                "2026-02-16T09:00:00-05:00",
            ),
            (
                "0 9 * * MON#2",
                "2026-02-01T07:00:00-05:00",
                "2026-02-09T09:00:00-05:00",
            ),
            (
                "0 9 * * 5#L",
                "2026-02-01T07:00:00-05:00",
                "2026-02-27T09:00:00-05:00",
            ),
            (
                "0 9 1 * +MON",
                "2026-02-02T07:00:00-05:00",
                "2026-06-01T09:00:00-04:00",
            ),
            (
                "30 2 * * *",
                "2026-03-08T01:00:00-05:00",
                "2026-03-08T03:00:00-04:00",
            ),
            (
                "30 1 * * *",
                "2026-11-01T01:30:00-04:00",
                "2026-11-02T01:30:00-05:00",
            ),
            (
                "*/15 1 * * *",
                "2026-11-01T01:45:00-04:00",
                "2026-11-01T01:00:00-05:00",
            ),
        ] {
            let start = ScheduleTime(format!("{now}[America/New_York]").parse::<ZonedDateTime>()?);
            let next_at = parser()
                .parse(schedule)?
                .find_next_occurrence(&start, false)?;
            assert_eq!(
                next_at.0.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
                next,
                "{schedule}"
            );
        }
        assert!(parser().parse("0 0 * * * 2027").is_err());
        Ok(())
    }
}

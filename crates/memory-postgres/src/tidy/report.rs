use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use kernel::queue::WorkerRole;
use kernel::tidy::TidyError;
use kernel::tidy::WorkerReport;
use kernel::tidy::WorkerReportStore;
use strum::IntoEnumIterator;

use super::failed;
use super::stored;
use crate::PostgresMemory;

type ReportRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[async_trait]
impl WorkerReportStore for PostgresMemory {
    async fn report(&self, reports: &[WorkerReport]) -> Result<(), TidyError> {
        let column = |field: fn(&WorkerReport) -> Option<String>| -> Vec<Option<String>> {
            reports.iter().map(field).collect()
        };
        sqlx::query(
            "INSERT INTO rh_worker_reports (node, role, provider, model, outage, reported_at)
             SELECT node, role, provider, model, outage, now()
               FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[])
                    AS report(node, role, provider, model, outage)
             ON CONFLICT (node, role) DO UPDATE
                SET provider = EXCLUDED.provider, model = EXCLUDED.model,
                    outage = EXCLUDED.outage, reported_at = EXCLUDED.reported_at",
        )
        .bind(column(|report| Some(report.node.clone())))
        .bind(column(|report| Some(report.role.to_string())))
        .bind(column(|report| report.provider.clone()))
        .bind(column(|report| report.model.clone()))
        .bind(column(|report| report.outage.clone()))
        .execute(&self.pool)
        .await
        .map_err(failed("worker report"))?;
        Ok(())
    }

    async fn newest_first(&self, fresh: Duration) -> Result<Vec<WorkerReport>, TidyError> {
        let known: Vec<String> = WorkerRole::iter().map(|role| role.to_string()).collect();
        let rows: Vec<ReportRow> = sqlx::query_as(
            "SELECT node, role, provider, model, outage FROM rh_worker_reports
              WHERE reported_at >= now() - make_interval(secs => $1) AND role = ANY($2)
              ORDER BY reported_at DESC, node, role",
        )
        .bind(fresh.as_secs_f64())
        .bind(known)
        .fetch_all(&self.pool)
        .await
        .map_err(failed("worker reports"))?;
        rows.into_iter()
            .map(|(node, role, provider, model, outage)| {
                Ok(WorkerReport {
                    role: WorkerRole::from_str(&role)
                        .map_err(|source| stored("worker report role")(Box::new(source)))?,
                    node,
                    provider,
                    model,
                    outage,
                })
            })
            .collect()
    }
}

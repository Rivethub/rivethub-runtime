use std::time::Duration;

use kernel::queue::WorkerRole;
use kernel::tidy::WorkerReport;
use kernel::tidy::WorkerReportStore as _;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn report(node: &str, role: WorkerRole, model: &str, outage: Option<&str>) -> WorkerReport {
    WorkerReport {
        node: node.to_owned(),
        role,
        provider: Some("http://models.example/v1".to_owned()),
        model: Some(model.to_owned()),
        outage: outage.map(str::to_owned),
        index_key: Some(format!("key-of-{model}")),
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_worker_report_replaces_the_last_one_and_expires(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let fresh = Duration::from_secs(120);
    store
        .report(&[
            report("datahub", WorkerRole::Summarizer, "flash", None),
            report("datahub", WorkerRole::Categorizer, "flash", None),
        ])
        .await?;
    store
        .report(&[report(
            "datahub",
            WorkerRole::Categorizer,
            "deepseek",
            Some("unreachable"),
        )])
        .await?;

    let mut reports = store.newest_first(fresh).await?;
    reports.sort_by_key(|report| report.role.to_string());
    assert_eq!(
        reports,
        [
            report(
                "datahub",
                WorkerRole::Categorizer,
                "deepseek",
                Some("unreachable")
            ),
            report("datahub", WorkerRole::Summarizer, "flash", None),
        ]
    );

    sqlx::query(
        "UPDATE rh_worker_reports SET reported_at = now() - interval '10 minutes'
          WHERE role = 'summarizer'",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_worker_reports (node, role, reported_at) VALUES ('later', 'a-newer-role', now())",
    )
    .execute(&pool)
    .await?;
    let reports = store.newest_first(fresh).await?;
    assert_eq!(
        reports,
        [report(
            "datahub",
            WorkerRole::Categorizer,
            "deepseek",
            Some("unreachable")
        )]
    );
    Ok(())
}

use std::sync::Arc;

use memory_postgres::Error;
use memory_postgres::PostgresMemory;
use memory_postgres::SCHEMA_VERSION;
use parking_lot::Mutex;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn url(pool: &PgPool) -> String {
    pool.connect_options().to_url_lossy().to_string()
}

#[sqlx::test(migrations = false)]
async fn a_fresh_database_is_not_adopted_and_left_untouched(pool: PgPool) -> TestResult {
    assert_eq!(SCHEMA_VERSION, 54);
    match PostgresMemory::connect(&url(&pool)).await {
        Err(error @ Error::NotAdopted) => assert_eq!(
            error.to_string(),
            "this database has no RivetHub migration history — run rh db migrate once to adopt it"
        ),
        other => panic!("expected NotAdopted, got {other:?}"),
    }
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 0, "the runtime must not create anything");
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn an_empty_migration_table_is_schema_behind(pool: PgPool) -> TestResult {
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    match PostgresMemory::connect(&url(&pool)).await {
        Err(Error::SchemaBehind { found, required }) => {
            assert_eq!(found, 0);
            assert_eq!(required, SCHEMA_VERSION);
        }
        other => panic!("expected SchemaBehind, got {other:?}"),
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_migrated_database_connects(pool: PgPool) -> TestResult {
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_newer_schema_is_accepted_with_one_warning(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, execution_time, checksum)
         VALUES (9999, 'fake-newer', true, 0, '\\x'::bytea)",
    )
    .execute(&pool)
    .await?;
    let (guard, lines) = warnings();
    PostgresMemory::connect(&url(&pool)).await?;
    drop(guard);
    let lines = lines.lock().clone();
    let [line] = lines.as_slice() else {
        return Err(format!("exactly one rolling-update warning, got {lines:?}").into());
    };
    assert!(
        line.contains(
            "the memory schema reports version 9999, this build requires 54 — rolling updates migrate first; continuing"
        ),
        "the warning must name the rolling update: {line}"
    );
    Ok(())
}

struct WarningRecorder(Arc<Mutex<Vec<String>>>);

struct MessageVisitor(Arc<Mutex<Vec<String>>>);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.lock().push(format!("{value:?}"));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for WarningRecorder
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() == tracing::Level::WARN {
            event.record(&mut MessageVisitor(self.0.clone()));
        }
    }
}

fn warnings() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let subscriber = tracing_subscriber::registry().with(WarningRecorder(lines.clone()));
    (tracing::subscriber::set_default(subscriber), lines)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_failed_newer_row_is_not_a_schema_version(pool: PgPool) -> TestResult {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, execution_time, checksum)
         VALUES (9999, 'failed-newer', false, 0, '\\x'::bytea)",
    )
    .execute(&pool)
    .await?;
    PostgresMemory::connect(&url(&pool)).await?;
    Ok(())
}

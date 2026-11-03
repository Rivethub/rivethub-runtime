#![cfg(test)]

use std::time::Duration;

use cli::doctor::{Check, CheckName, CheckStatus, DoctorReport};
use cli::{Cli, Dispatch, Exit, Report};
use config::Environment;
use sqlx::ConnectOptions;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn setup(extra: &str) -> (TempDir, Environment) {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir(home.path().join(".rivethub")).expect("state");
    let text = format!(
        "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n{extra}"
    );
    std::fs::write(home.path().join(".rivethub/config.yaml"), text).expect("configuration");
    let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    (home, env)
}

async fn output(env: &Environment) -> Report {
    let command = Cli::parse_args(["rh", "doctor", "--json"].map(Into::into)).expect("doctor args");
    let shutdown = CancellationToken::new();
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        cli::dispatch(command, env, &shutdown),
    )
    .await
    .expect("doctor deadline");
    let Dispatch::Complete(report) = output else {
        panic!("unexpected service start")
    };
    report
}

async fn doctor(env: &Environment) -> (Exit, DoctorReport) {
    let output = output(env).await;
    let report: DoctorReport = serde_json::from_str(&output.stdout).expect("typed doctor report");
    assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report.summary.pass + report.summary.warn + report.summary.fail,
        report.checks.len()
    );
    (output.exit, report)
}

fn memory(report: &DoctorReport) -> &Check {
    report
        .checks
        .iter()
        .find(|check| check.name == CheckName::Memory)
        .expect("memory check")
}

#[tokio::test]
async fn sqlite_doctor_checks_current_and_behind_without_migrating() {
    let db = tempfile::tempdir().expect("database directory");
    let path = db.path().join("memory.sqlite");
    let (_home, env) = setup(&format!(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
        path.display()
    ));
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Failure);
    assert_eq!(memory(&report).status, CheckStatus::Fail);
    assert_eq!(
        memory(&report).message,
        "Schema 0 is not current; required 30, pending 30. Run rivethub-migrate explicitly."
    );
    assert!(!path.exists(), "doctor must not create missing stores");
    migrate::sqlite(&path).expect("test schema");
    let before = std::fs::read(&path).expect("before");
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Ok, "{report:?}");
    assert_eq!(memory(&report).status, CheckStatus::Pass);
    assert_eq!(memory(&report).message, "Schema 30 is current.");
    assert_eq!(std::fs::read(&path).expect("after"), before);
    let connection = rusqlite::Connection::open(&path).expect("fixture database");
    connection
        .pragma_update(None, "user_version", 6)
        .expect("fixture version");
    drop(connection);
    let before = std::fs::read(&path).expect("before old schema");
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Failure);
    assert_eq!(memory(&report).status, CheckStatus::Fail);
    assert_eq!(
        memory(&report).message,
        "Schema 6 is not current; required 30, pending 24. Run rivethub-migrate explicitly."
    );
    assert_eq!(std::fs::read(&path).expect("after old schema"), before);
}

#[sqlx::test(migrations = false)]
async fn postgres_doctor_checks_current_and_behind_without_migrating(pool: sqlx::PgPool) {
    let url = pool.connect_options().to_url_lossy().to_string();
    let (home, _) = setup(
        "memory:\n  backend: postgres\n  postgres:\n    connection_string: ${RIVETHUB_PG_URL}\n",
    );
    let env = Environment::from_vars([
        (
            "HOME".to_owned(),
            home.path().to_str().expect("home").to_owned(),
        ),
        (config::POSTGRES_URL_VARIABLE.to_owned(), url),
    ]);
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Failure);
    assert_eq!(memory(&report).status, CheckStatus::Fail);
    assert_eq!(
        migrate::postgres_status_pool(&pool)
            .await
            .expect("schema status")
            .current,
        0
    );
    migrate::postgres_pool(&pool).await.expect("test schema");
    let before = migrate::postgres_status_pool(&pool).await.expect("before");
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Ok, "{report:?}");
    assert_eq!(memory(&report).status, CheckStatus::Pass);
    assert_eq!(
        migrate::postgres_status_pool(&pool).await.expect("after"),
        before
    );
}

#[tokio::test]
async fn terminal_check_accepts_executable_paths_with_spaces_and_refuses_non_executables() {
    use std::os::unix::fs::PermissionsExt;
    let (home, _) = setup("gateway:\n  terminal:\n    enabled: true\n");
    let bin = home.path().join("bin with spaces");
    std::fs::create_dir(&bin).expect("bin");
    let env = Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        ("PATH".as_ref(), bin.as_os_str()),
    ]);
    let (_, report) = doctor(&env).await;
    assert_eq!(
        report.checks.last().expect("herdr").status,
        CheckStatus::Fail
    );
    let executable = bin.join("herdr");
    std::fs::write(&executable, "test executable marker").expect("herdr marker");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600))
        .expect("non-executable");
    let (_, report) = doctor(&env).await;
    assert_eq!(
        report.checks.last().expect("herdr").status,
        CheckStatus::Fail
    );
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("executable");
    let (_, report) = doctor(&env).await;
    let check = report.checks.last().expect("herdr");
    assert_eq!(check.name, CheckName::Herdr);
    assert_eq!(check.status, CheckStatus::Pass);
    let loop_path = home.path().join("loop");
    std::os::unix::fs::symlink(&loop_path, &loop_path).expect("unreadable PATH entry");
    let env = Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        ("PATH".as_ref(), loop_path.as_os_str()),
    ]);
    let (_, report) = doctor(&env).await;
    assert_eq!(
        report.checks.last().expect("herdr error").status,
        CheckStatus::Fail
    );
}

#[tokio::test]
async fn doctor_reports_missing_state_and_invalid_config_as_failures() {
    let (home, env) = setup("");
    std::fs::remove_dir_all(home.path().join(".rivethub")).expect("remove state");
    let (exit, report) = doctor(&env).await;
    assert_eq!(exit, Exit::Failure);
    assert_eq!(report.summary.fail, 4);
    assert!(!home.path().join(".rivethub").exists());
}

#[tokio::test]
async fn deprecated_alias_notice_is_reported_once_on_success_and_failure() {
    let db = tempfile::tempdir().expect("database directory");
    let path = db.path().join("memory.sqlite");
    migrate::sqlite(&path).expect("test database");
    let (home, _) = setup(&format!(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
        path.display()
    ));
    let env = Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        (
            "RIVETOS_PG_URL".as_ref(),
            "postgres://127.0.0.1:1/unused".as_ref(),
        ),
    ]);
    for corrupt in [false, true] {
        if corrupt {
            std::fs::write(&path, "invalid database").expect("corrupt fixture");
        }
        let report = output(&env).await;
        let complete = format!("{}{}", report.stdout, report.stderr);
        assert_eq!(
            complete.matches("RIVETOS_PG_URL is deprecated").count(),
            1,
            "{complete}"
        );
        assert_eq!(report.exit, if corrupt { Exit::Failure } else { Exit::Ok });
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn absent_memory_section_uses_the_environment_postgres_url(pool: sqlx::PgPool) {
    let (home, _) = setup("");
    let url = pool.connect_options().to_url_lossy().to_string();
    let env = Environment::from_vars([
        ("HOME".as_ref(), home.path().as_os_str()),
        (config::POSTGRES_URL_VARIABLE.as_ref(), url.as_ref()),
    ]);
    let (_, report) = doctor(&env).await;
    assert_eq!(memory(&report).status, CheckStatus::Pass);
    assert_eq!(memory(&report).message, "Schema 52 is current.");
}

#[tokio::test]
async fn pending_import_overrides_explicit_postgres_in_doctor() {
    let (_home, env) = setup(
        "memory:\n  backend: postgres\n  postgres:\n    embedded:\n      data_dir: /home/user/import\n",
    );
    let (_, report) = doctor(&env).await;
    assert_eq!(memory(&report).status, CheckStatus::Fail);
    assert_eq!(memory(&report).message, cli::Error::SqlitePath.to_string());
}

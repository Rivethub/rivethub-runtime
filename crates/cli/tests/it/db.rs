use crate::support;

use cli::Exit;
use sqlx::ConnectOptions as _;
use support::{config, environment, home, run};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn sqlite_status_is_read_only_and_migrate_is_idempotent() -> Result {
    let home = home();
    let env = environment(&home);
    let path = home.path().join("memory with spaces.sqlite");
    let path_text = path.to_str().ok_or("path")?;
    let before = run(&["db", "status", "--path", path_text], &env).await;
    assert_eq!(before.exit, Exit::Ok);
    assert_eq!(
        before.stdout,
        format!("sqlite {path_text}: current 0, required 30, pending 30\nforeign: none\n")
    );
    assert!(!path.exists());
    for _ in 0..2 {
        let applied = run(&["db", "migrate", "--path", path_text], &env).await;
        assert_eq!(applied.exit, Exit::Ok, "{}", applied.stderr);
        assert_eq!(
            applied.stdout,
            format!("sqlite {path_text}: applied; current 30, required 30, pending 0\n")
        );
    }
    let bytes = std::fs::read(&path)?;
    let after = run(&["db", "status", "--path", path_text], &env).await;
    assert_eq!(
        after.stdout,
        format!("sqlite {path_text}: current 30, required 30, pending 0\nforeign: none\n")
    );
    assert_eq!(std::fs::read(path)?, bytes);
    Ok(())
}

#[tokio::test]
async fn configured_database_missing_config_and_typed_flags() -> Result {
    let home = home();
    let env = environment(&home);
    let missing = run(&["db", "status"], &env).await;
    assert_eq!(missing.exit, Exit::Failure);
    assert_eq!(
        missing.stderr,
        "Error: No config found. Run: rh config init\n"
    );
    let path = home.path().join("db.sqlite");
    config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            path.display()
        ),
    );
    assert_eq!(run(&["db", "migrate"], &env).await.exit, Exit::Ok);
    assert!(path.exists());
    for args in [
        vec!["db", "wrong"],
        vec![
            "db",
            "status",
            "--path",
            "x",
            "--url",
            "postgres://example.com/db",
        ],
        vec!["update", "--channel", "wrong"],
        vec!["update", "--ssh-user", "user"],
    ] {
        assert_eq!(run(&args, &env).await.exit, Exit::Failure);
    }
    for command in ["update", "db"] {
        assert_eq!(run(&[command, "--help"], &env).await.exit, Exit::Ok);
    }
    Ok(())
}

#[sqlx::test]
async fn postgres_commands_use_the_isolated_database(pool: sqlx::PgPool) -> Result {
    let home = home();
    let env = environment(&home);
    let url = pool.connect_options().to_url_lossy().to_string();
    let before = run(&["db", "status", "--url", &url], &env).await;
    assert_eq!(before.exit, Exit::Ok, "{}", before.stderr);
    let required = migrate::required()?;
    assert_eq!(
        before.stdout,
        format!(
            "postgres: current 0, required {required}, pending {}\n",
            migrate::POSTGRES.migrations.len()
        )
    );
    for _ in 0..2 {
        let applied = run(&["db", "migrate", "--url", &url], &env).await;
        assert_eq!(applied.exit, Exit::Ok, "{}", applied.stderr);
        assert_eq!(
            applied.stdout,
            format!("postgres: applied; current {required}, required {required}, pending 0\n")
        );
    }
    let after = run(&["db", "status", "--url", &url], &env).await;
    assert_eq!(
        after.stdout,
        format!("postgres: current {required}, required {required}, pending 0\n")
    );
    Ok(())
}

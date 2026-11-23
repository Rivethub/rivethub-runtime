use std::sync::Arc;

use kernel::tenancy::{DEFAULT_SPACE, SpaceKind, TenancyStore};
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wire::gateway::users::UsersRegistry;

use super::{Identity, keep_spaces, load, registry};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const LEGACY_NAME: &str = "Household";
const CUSTOM_NAME: &str = "Workshop";
const STORED_KIND: &str = "household";

async fn startup_sync(store: Arc<dyn TenancyStore>, identity: &Identity) -> TestResult {
    let mut tasks = JoinSet::new();
    let shutdown = CancellationToken::new();
    keep_spaces(
        &mut tasks,
        Some(store),
        identity,
        "node-a".into(),
        &shutdown,
    );
    shutdown.cancel();
    tasks
        .join_next()
        .await
        .ok_or("space sync did not start")???;
    Ok(())
}

fn check_row(row: &(String, String), name: &str) -> TestResult {
    assert_eq!(row.0, STORED_KIND);
    assert_eq!(SpaceKind::parse(&row.0)?, SpaceKind::Device);
    assert_eq!(row.1, name);
    Ok(())
}

#[tokio::test]
async fn sqlite_startup_names_a_one_person_space_and_preserves_a_custom_name() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(memory_sqlite::SqliteStore::open(&path)?);
    let conn = rusqlite::Connection::open(&path)?;
    let identity = registry(UsersRegistry::default())?;
    let space = identity
        .tenancy
        .spaces()
        .first()
        .ok_or("no default space")?;
    assert_eq!(space.name, "This device");
    assert_eq!(space.members.len(), 1);
    conn.execute(
        "INSERT INTO rh_spaces (id, kind, name) VALUES (?1, ?2, ?3)",
        [DEFAULT_SPACE, STORED_KIND, LEGACY_NAME],
    )?;
    for name in ["This device", CUSTOM_NAME] {
        startup_sync(store.clone(), &identity).await?;
        let row = conn.query_row(
            "SELECT kind, name FROM rh_spaces WHERE id = ?1",
            [DEFAULT_SPACE],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        check_row(&row, name)?;
        conn.execute(
            "UPDATE rh_spaces SET name = ?1 WHERE id = ?2",
            [CUSTOM_NAME, DEFAULT_SPACE],
        )?;
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_startup_names_a_multi_person_space_and_preserves_a_custom_name(
    pool: PgPool,
) -> TestResult {
    let store = Arc::new(memory_postgres::PostgresTenancy::new(pool.clone()));
    let identity = registry(serde_json::from_str(
        r#"{"ownerUserId":"alice","unmappedIsOwner":false,"users":{"alice":{},"bob":{}}}"#,
    )?)?;
    let space = identity
        .tenancy
        .spaces()
        .first()
        .ok_or("no default space")?;
    assert_eq!(space.name, "This hub");
    assert_eq!(space.members.len(), 2);
    for keeper in ["node-a", "node-b"] {
        sqlx::query("UPDATE rh_membership_authority SET node = $1")
            .bind(keeper)
            .execute(&pool)
            .await?;
        for (before, after) in [(LEGACY_NAME, "This hub"), (CUSTOM_NAME, CUSTOM_NAME)] {
            sqlx::query(
                "INSERT INTO rh_spaces (id, kind, name) VALUES ($1, $2, $3)
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            )
            .bind(DEFAULT_SPACE)
            .bind(STORED_KIND)
            .bind(before)
            .execute(&pool)
            .await?;
            startup_sync(store.clone(), &identity).await?;
            let row = sqlx::query_as("SELECT kind, name FROM rh_spaces WHERE id = $1")
                .bind(DEFAULT_SPACE)
                .fetch_one(&pool)
                .await?;
            check_row(&row, after)?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_users_file_accepts_both_device_kind_spellings() -> TestResult {
    let home = tempfile::tempdir()?;
    let path = home.path().join("users.json");
    let config = config::Config {
        gateway: Some(config::model::GatewaySection {
            users_file: Some(path.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    for kind in ["device", STORED_KIND] {
        tokio::fs::write(
            &path,
            format!(
                r#"{{"ownerUserId":"alice","unmappedIsOwner":false,"users":{{}},"spaces":{{"studio":{{"kind":"{kind}","members":["alice"]}}}}}}"#
            ),
        )
        .await?;
        let identity = load(&config, &config::Environment::default()).await?;
        let space = identity
            .tenancy
            .spaces()
            .first()
            .ok_or("no configured space")?;
        assert_eq!(space.kind, SpaceKind::Device);
        assert_eq!(SpaceKind::parse(kind)?, SpaceKind::Device);
    }
    Ok(())
}

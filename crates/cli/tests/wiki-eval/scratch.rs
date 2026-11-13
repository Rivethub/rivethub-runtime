use std::time::Duration;

use sqlx::PgPool;

use crate::failure::Failure;
use crate::failure::database;
use crate::failure::redact;

pub(crate) struct Databases {
    pub(crate) admin: String,
    pub(crate) app: String,
}

pub(crate) fn databases(scratch: &str, db: &str) -> Result<Databases, Failure> {
    let mut url = url::Url::parse(scratch).map_err(|error| Failure::Usage {
        detail: format!("scratch url: {error}"),
    })?;
    if url.host_str().is_none() {
        return Err(Failure::Usage {
            detail: "scratch url needs a host".to_owned(),
        });
    }
    url.set_path("postgres");
    let admin = url.to_string();
    url.set_path(db);
    Ok(Databases {
        admin,
        app: url.to_string(),
    })
}

pub(crate) fn name(label: &str) -> String {
    let tail: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!("wiki_eval_{tail}")
}

pub(crate) async fn fresh(admin: &str, db: &str, secrets: &[String]) -> Result<(), Failure> {
    let pool = PgPool::connect(admin)
        .await
        .map_err(|error| database(&error, secrets))?;
    let mut last = None;
    for attempt in 0..3 {
        let terminated = sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
        )
        .bind(db)
        .execute(&pool)
        .await;
        if let Err(error) = terminated {
            last = Some(database(&error, secrets));
            continue;
        }
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(sleep, reason = "the wait is the fixture")
        )]
        if let Err(error) = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS \"{db}\""
        )))
        .execute(&pool)
        .await
        {
            last = Some(database(&error, secrets));
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(sleep, reason = "the wait is the fixture")
        )]
        match sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{db}\"")))
            .execute(&pool)
            .await
        {
            Ok(_) => {
                pool.close().await;
                return Ok(());
            }
            Err(error) if attempt < 2 => {
                last = Some(database(&error, secrets));
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => last = Some(database(&error, secrets)),
        }
    }
    pool.close().await;
    Err(last.unwrap_or_else(|| Failure::Database(format!("could not recreate {db}"))))
}

pub(crate) async fn migrate(app: &str, secrets: &[String]) -> Result<(), Failure> {
    migrate::postgres(app)
        .await
        .map(|_| ())
        .map_err(|error| Failure::Migrate(redact(&error.to_string(), secrets)))
}

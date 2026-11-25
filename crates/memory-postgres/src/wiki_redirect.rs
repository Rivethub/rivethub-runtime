use kernel::wiki::builder::WikiStoreError;
use sqlx::{Connection as _, PgConnection};

const HOLD_ATTEMPTS: usize = 3;

fn failed(error: sqlx::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(error))
}

async fn check_retire(conn: &mut PgConnection, page: &str) -> Result<(), WikiStoreError> {
    let aliases: Vec<String> = sqlx::query_scalar(
        "SELECT from_slug FROM rh_wiki_redirects
          WHERE to_slug = $1
            AND row_security_active('rh_wiki_redirects')
            AND NOT coalesce((SELECT rh_scope_maintenance())
                             OR owner_user_id = (SELECT rh_scope_principal()), false)
          ORDER BY from_slug",
    )
    .bind(page)
    .fetch_all(&mut *conn)
    .await
    .map_err(failed)?;
    if aliases.is_empty() {
        return Ok(());
    }
    Err(WikiStoreError::RedirectsHeld {
        page: page.to_owned(),
        aliases,
    })
}

async fn lock_redirects(
    conn: &mut PgConnection,
    retired: &[&str],
    into: Option<&str>,
    wait: bool,
) -> Result<(), sqlx::Error> {
    let query = if wait {
        "SELECT 1 FROM rh_wiki_redirects
          WHERE to_slug = ANY($1) OR from_slug = ANY($1) OR from_slug = $2
          ORDER BY from_slug FOR UPDATE"
    } else {
        "SELECT 1 FROM rh_wiki_redirects
          WHERE to_slug = ANY($1) OR from_slug = ANY($1) OR from_slug = $2
          ORDER BY from_slug FOR UPDATE NOWAIT"
    };
    sqlx::query(query)
        .bind(retired)
        .bind(into)
        .execute(&mut *conn)
        .await
        .map(drop)
}

fn busy(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "55P03")
}

async fn hold_once(
    conn: &mut PgConnection,
    retired: &[&str],
    into: Option<&str>,
) -> Result<bool, WikiStoreError> {
    let mut savepoint = conn.begin().await.map_err(failed)?;
    lock_redirects(&mut savepoint, retired, into, true)
        .await
        .map_err(failed)?;
    sqlx::query("SELECT 1 FROM rh_wiki_topics WHERE slug = ANY($1) ORDER BY slug FOR UPDATE")
        .bind(retired)
        .execute(&mut *savepoint)
        .await
        .map_err(failed)?;
    match lock_redirects(&mut savepoint, retired, into, false).await {
        Ok(()) => {
            savepoint.commit().await.map_err(failed)?;
            Ok(true)
        }
        Err(error) if busy(&error) => {
            savepoint.rollback().await.map_err(failed)?;
            Ok(false)
        }
        Err(error) => Err(failed(error)),
    }
}

pub(crate) async fn hold_pages(
    conn: &mut PgConnection,
    retired: &[&str],
    into: Option<&str>,
) -> Result<(), WikiStoreError> {
    for _ in 0..HOLD_ATTEMPTS {
        if hold_once(conn, retired, into).await? {
            for page in retired {
                check_retire(conn, page).await?;
            }
            return Ok(());
        }
    }
    Err(WikiStoreError::RedirectsMoving(
        retired.iter().map(|page| (*page).to_owned()).collect(),
    ))
}

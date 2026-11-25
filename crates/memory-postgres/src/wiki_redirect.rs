use kernel::wiki::builder::WikiStoreError;
use sqlx::PgConnection;

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

pub(crate) async fn hold_pages(
    conn: &mut PgConnection,
    retired: &[&str],
    into: Option<&str>,
) -> Result<(), WikiStoreError> {
    sqlx::query(
        "SELECT 1 FROM rh_wiki_redirects
          WHERE to_slug = ANY($1) OR from_slug = ANY($1) OR from_slug = $2
          ORDER BY from_slug FOR UPDATE",
    )
    .bind(retired)
    .bind(into)
    .execute(&mut *conn)
    .await
    .map_err(failed)?;
    sqlx::query("SELECT 1 FROM rh_wiki_topics WHERE slug = ANY($1) ORDER BY slug FOR UPDATE")
        .bind(retired)
        .execute(&mut *conn)
        .await
        .map_err(failed)?;
    for page in retired {
        check_retire(conn, page).await?;
    }
    Ok(())
}

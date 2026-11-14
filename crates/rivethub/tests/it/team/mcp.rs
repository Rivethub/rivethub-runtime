use std::sync::Arc;

use kernel::owner::Owner;
use kernel::tenancy::{Scope, TenancyStore as _};
use memory_postgres::{PostgresMemory, PostgresTenancy};
use reqwest::Method;
use rmcp::model::CallToolRequestParams;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

use super::sessions::{MARKER, PHILS, TOMS, seeded};
use super::{ALEX, Hub, PHIL, TestResult, segment};

const PINNED: &str = "rh_team_alex";

async fn pinned_login(
    hub: &Hub,
    person: &str,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    let admin = hub.db.pool();
    for statement in [
        format!(
            "DO $$ BEGIN CREATE ROLE {PINNED} LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '{PINNED}'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        ),
        format!("GRANT USAGE ON SCHEMA public TO {PINNED}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {PINNED}"),
        format!("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {PINNED}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&admin)
            .await?;
    }
    sqlx::query("INSERT INTO rh_role_principals (role_name, user_id) VALUES ($1, $2)")
        .bind(PINNED)
        .bind(person)
        .execute(&admin)
        .await?;
    let options = admin
        .connect_options()
        .as_ref()
        .clone()
        .username(PINNED)
        .password(PINNED);
    let pool = memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
        .connect_with(options)
        .await?;
    Ok(PostgresMemory::new(pool))
}

async fn recalled(
    server: &mcp::Server,
    scope: &Scope,
    tool: &str,
    arguments: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut request = CallToolRequestParams::new(tool.to_owned());
    request.arguments = Some(serde_json::from_str(arguments)?);
    let result = scope.clone().run(server.call(request)).await?;
    assert_eq!(result.is_error, None, "{tool} answered");
    let mut text = String::new();
    for content in &result.content {
        if let Some(part) = content.as_text() {
            text.push_str(&part.text);
        }
    }
    Ok(text)
}

async fn recalls(
    server: &mcp::Server,
    scope: &Scope,
    phrase: &str,
) -> Result<[bool; 2], Box<dyn std::error::Error>> {
    let search = recalled(
        server,
        scope,
        "memory_search",
        &format!(r#"{{"query":"{MARKER}","mode":"fts","scope":"messages","limit":50}}"#),
    )
    .await?;
    let browse = recalled(server, scope, "memory_browse", r#"{"limit":200}"#).await?;
    Ok([search.contains(phrase), browse.contains(phrase)])
}

#[tokio::test]
async fn a_pinned_login_recalls_what_its_person_may_read_and_nothing_trashed() -> TestResult {
    let hub = seeded().await?;
    let memory = Arc::new(pinned_login(&hub, ALEX).await?);
    let tenancy = PostgresTenancy::new(memory.pool().clone());
    let alex = Scope::Viewer(tenancy.viewer_of(&Owner::new(ALEX.to_owned())?).await?);
    let server = alex
        .clone()
        .enter(|| mcp::Server::new(memory.clone(), false, CancellationToken::new()))?;
    let phils = "phil asks how the";
    let toms = "tom keeps his notes";
    assert_eq!(
        recalls(&server, &alex, phils).await?,
        [true; 2],
        "a teammate's shared session"
    );
    assert_eq!(
        recalls(&server, &alex, toms).await?,
        [false; 2],
        "a teammate's private session"
    );
    let forged = Scope::Viewer(kernel::tenancy::Viewer::new(
        Owner::new("tom".to_owned())?,
        Vec::new(),
    ));
    assert_eq!(
        recalls(&server, &forged, toms).await?,
        [false; 2],
        "a pinned login keeps its own person whatever scope it is given"
    );
    let trash = format!("/api/sessions/{}/trash", segment(PHILS)?);
    assert_eq!(
        hub.call(PHIL, Method::POST, &trash, None).await?.status,
        204
    );
    assert_eq!(
        recalls(&server, &alex, phils).await?,
        [false; 2],
        "the owner trashed it"
    );
    let refused = format!("/api/sessions/{}/restore", segment(TOMS)?);
    assert_eq!(
        hub.call(ALEX, Method::POST, &refused, None).await?.status,
        404
    );
    let restore = format!("/api/sessions/{}/restore", segment(PHILS)?);
    assert_eq!(
        hub.call(PHIL, Method::POST, &restore, None).await?.status,
        204
    );
    assert_eq!(
        recalls(&server, &alex, phils).await?,
        [true; 2],
        "the owner restored it"
    );
    memory.pool().close().await;
    hub.stop().await
}

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use kernel::memory::MemoryStore as _;
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use serde::Deserialize;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PROBE: &str = "rh_mcp_probe";

#[derive(Deserialize)]
struct Recording {
    calls: Vec<Call>,
}

#[derive(Deserialize)]
struct Call {
    request: CallToolRequestParams,
    note: String,
    result: CallToolResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExceptionKind {
    TypedRejection,
    Unresolved,
}

#[derive(Deserialize)]
struct Exception {
    tool: String,
    note: String,
    kind: ExceptionKind,
    reason: String,
}

fn exceptions() -> Result<Vec<Exception>, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(
        testkit::recording("mcp-memory/rust-replay-exceptions.json"),
    )?)?)
}

#[test]
fn replay_exceptions_name_unique_known_calls_and_explain_each_difference() -> TestResult {
    let calls = recording()?.calls;
    assert_eq!(calls.len(), 73);
    let mut names = BTreeSet::new();
    for exception in exceptions()? {
        assert!(names.insert((exception.tool.clone(), exception.note.clone())));
        assert_ne!(exception.reason.trim(), "");
        assert!(
            calls
                .iter()
                .any(|call| call.request.name == exception.tool && call.note == exception.note)
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct ContractStamp {
    #[serde(rename = "contractVersion")]
    contract_version: u32,
}

fn contract_stamp(
    meta: Option<&rmcp::model::MetaObject>,
) -> Result<u32, Box<dyn std::error::Error>> {
    let meta = meta.ok_or("memory tool result is missing contract meta")?;
    let stamp: ContractStamp = wire::JsonObject::from_serialize(meta)?.deserialize()?;
    Ok(stamp.contract_version)
}

fn meta_without_contract(
    meta: Option<&rmcp::model::MetaObject>,
) -> Result<Option<wire::JsonObject>, Box<dyn std::error::Error>> {
    let Some(meta) = meta else {
        return Ok(None);
    };
    let mut object = wire::JsonObject::from_serialize(meta)?;
    object.retain(|key| key != "contractVersion");
    if object.as_map().is_empty() {
        Ok(None)
    } else {
        Ok(Some(object))
    }
}

fn recording() -> Result<Recording, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(
        testkit::recording("mcp-memory/calls.json"),
    )?)?)
}

async fn seed(pool: &PgPool) -> TestResult {
    let legacy = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            migrate::POSTGRES
                .iter()
                .filter(|migration| migration.version <= 20)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    legacy.run(pool).await?;
    sqlx::raw_sql(include_str!(
        "../../../testkit/tests/fixtures/mcp-memory/seed.txt"
    ))
    .execute(pool)
    .await?;
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT set_config('rh.store_owner', $1, false)")
        .bind(testkit::OWNER)
        .execute(&mut *conn)
        .await?;
    migrate::POSTGRES.run(&mut *conn).await?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario; the sqlx::test macro had hidden its length"
)]
async fn recorded_memory_text() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let wiki = tempfile::tempdir()?;
    std::fs::create_dir(wiki.path().join("topics"))?;
    for entry in std::fs::read_dir(testkit::recording("mcp-memory/shared/wiki/topics"))? {
        let entry = entry?;
        std::fs::copy(
            entry.path(),
            wiki.path().join("topics").join(entry.file_name()),
        )?;
    }
    let shutdown = CancellationToken::new();
    let server = mcp::Server::builder(
        Arc::new(
            memory_postgres::PostgresMemory::new(pool).with_wiki_directory(wiki.path().to_owned()),
        ),
        false,
        shutdown.clone(),
    )
    .display_zone(chrono_tz::Tz::UTC)
    .build()?;
    let (client_io, server_io) = tokio::io::duplex(65536);
    let task = tokio::spawn(mcp::transport::serve_io(
        server,
        server_io,
        shutdown.clone(),
    ));
    let client = ClientConfig::default().serve(client_io).await?;
    let mut failures = Vec::new();
    let exceptions = exceptions()?;
    for call in recording()?.calls {
        let exception = exceptions
            .iter()
            .find(|exception| exception.tool == call.request.name && exception.note == call.note);
        if exception.is_some_and(|exception| matches!(exception.kind, ExceptionKind::Unresolved)) {
            continue;
        }
        let result =
            tokio::time::timeout(Duration::from_secs(10), client.call_tool(call.request)).await??;
        if exception
            .is_some_and(|exception| matches!(exception.kind, ExceptionKind::TypedRejection))
        {
            assert_eq!(result.is_error, Some(true), "{}", call.note);
            continue;
        }
        assert_eq!(result.is_error, call.result.is_error, "{}", call.note);
        assert_eq!(result.result_type, call.result.result_type, "{}", call.note);
        assert_eq!(
            result.structured_content, call.result.structured_content,
            "{}",
            call.note
        );
        let stamp = contract_stamp(result.meta.as_ref())
            .map_err(|error| format!("{error} ({})", call.note))?;
        assert_eq!(stamp, wire::contract::MEMORY_CONTRACT, "{}", call.note);
        assert_eq!(
            meta_without_contract(result.meta.as_ref())?,
            meta_without_contract(call.result.meta.as_ref())?,
            "{}",
            call.note
        );
        assert_eq!(
            result.content.len(),
            call.result.content.len(),
            "{}",
            call.note
        );
        for (actual, expected) in result.content.iter().zip(&call.result.content) {
            let actual = actual.as_text().ok_or("actual text")?;
            let expected = expected.as_text().ok_or("expected text")?;
            assert_eq!(actual.meta, expected.meta, "{}", call.note);
            assert_eq!(actual.annotations, expected.annotations, "{}", call.note);
            if let Err(error) =
                testkit::mcp_text::compare_memory_text(&actual.text, &expected.text, false)
            {
                failures.push(format!("{}: {error}", call.note));
            }
        }
    }
    client.cancel().await?;
    shutdown.cancel();
    task.await??;
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    Ok(())
}

#[tokio::test]
async fn tool_calls_see_the_owner_rows_through_row_security() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    seed(&pool).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PROBE} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(&pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA public TO {PROBE}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(&pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("role", PROBE)]);
    let probe = memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
        .connect_with(options)
        .await?;
    let owner = Scope::Viewer(Viewer::new(
        Owner::new(testkit::OWNER.to_owned())?,
        Vec::new(),
    ));
    let shutdown = CancellationToken::new();
    let server = owner.clone().enter(|| {
        mcp::Server::new(
            Arc::new(memory_postgres::PostgresMemory::new(probe)),
            false,
            shutdown.clone(),
        )
    })?;
    let (client_io, server_io) = tokio::io::duplex(65536);
    let task = tokio::spawn(owner.run(mcp::transport::serve_io(
        server,
        server_io,
        shutdown.clone(),
    )));
    let client = ClientConfig::default().serve(client_io).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(CallToolRequestParams::new("memory_stats")),
    )
    .await??;
    let text = &result
        .content
        .first()
        .ok_or("content")?
        .as_text()
        .ok_or("text")?
        .text;
    assert!(text.contains("**Messages:** 20\n"), "{text}");
    client.cancel().await?;
    shutdown.cancel();
    task.await??;
    Ok(())
}

#[tokio::test]
async fn summary_tree_and_source_messages_render_from_adopted_rows() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let server = mcp::Server::new(
        Arc::new(memory_postgres::PostgresMemory::new(pool)),
        false,
        CancellationToken::new(),
    )?;
    let mut request = CallToolRequestParams::new("memory_search");
    request.arguments = Some(serde_json::from_str(
        r#"{"query":"Kiwi","mode":"regex","scope":"summaries"}"#,
    )?);
    let result = server.call(request).await?;
    assert_eq!(result.is_error, None);
    let text = &result
        .content
        .first()
        .ok_or("content")?
        .as_text()
        .ok_or("text")?
        .text;
    assert!(text.contains("Found 3 results (3 summaries, 0 messages)"));
    assert!(text.contains("### Summaries (expanded)"));
    assert!(text.contains("**[root]**"));
    assert!(text.contains("**[branch]**"));
    assert!(text.contains("**[leaf]**"));
    assert!(text.contains("period: 2026-09-02 → 2026-09-02) tags: project:Kiwi, topic:CI cache"));
    assert!(text.contains("**Children (1):**"));
    assert!(text.contains("**Source messages (4):**"));
    assert!(text.contains("  > [user] The Kiwi CI builds are taking 40 minutes again."));
    assert!(!text.contains("  > [tool]"));
    Ok(())
}

#[tokio::test]
async fn chunk_matches_keep_their_source_range_and_unicode_preview() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let id = "a0000000-0000-4000-8000-000000000003";
    let mut vector = vec![0.0_f32; 1024];
    *vector.first_mut().ok_or("vector dimension")? = 1.0;
    let opposite: Vec<_> = vector.iter().map(|value| -value).collect();
    let payload = "🦀".repeat(1501);
    sqlx::query(
        "UPDATE rh_messages SET content = 'prefix', tool_result = $1 WHERE id = $2::text::uuid",
    )
    .bind(&payload)
    .bind(id)
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO rh_message_chunks (message_id, idx, char_start, char_end, content, embedding, owner_user_id) VALUES ($1::text::uuid, 0, 0, 6, 'prefix', ($4::real[])::halfvec, 'owner'), ($1::text::uuid, 1, 6, 1507, $2, ($3::real[])::halfvec, 'owner')")
        .bind(id).bind(&payload).bind(&vector).bind(&opposite).execute(&pool).await?;
    let memory = Arc::new(memory_postgres::PostgresMemory::new(pool).with_query_embed(
        memory_postgres::QueryEmbed::new(
            Arc::new(testkit::FakeProvider::new(vec![testkit::Script::Embed(
                vec![kernel::model::Embedding { vector }],
            )])),
            None,
            None,
            None,
        ),
    ));
    let server = mcp::Server::new(memory.clone(), false, CancellationToken::new())?;
    let mut request = CallToolRequestParams::new("memory_search");
    request.arguments = Some(serde_json::from_str(
        r#"{"query":"unicode tool","mode":"vector","scope":"messages","limit":1}"#,
    )?);
    let result = server.call(request).await?;
    assert_eq!(result.is_error, None);
    let text = &result
        .content
        .first()
        .ok_or("content")?
        .as_text()
        .ok_or("text")?
        .text;
    assert!(text.contains(&format!(
        "…[chunk 2/2 · chars 6-1507 → memory_get_full id={id}]…\n{}…",
        "🦀".repeat(1500)
    )));
    assert!(!text.contains("display-truncated tool_result"));
    assert!(!text.contains(&payload));
    memory.close().await?;
    Ok(())
}

#[tokio::test]
async fn wiki_files_follow_redirects_and_refuse_sibling_traversal_and_symlinks() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    seed(&pool).await?;
    let root = tempfile::tempdir()?;
    let directory = root.path().join("topics");
    let sibling = root.path().join("topics-private");
    std::fs::create_dir(&directory)?;
    std::fs::create_dir(&sibling)?;
    std::fs::write(sibling.join("secret.md"), "private")?;
    let slug = "Acme's $(topic) 日本語";
    sqlx::query("INSERT INTO rh_wiki_topics (slug, title, current_state) VALUES ($1, 'Literal title', 'Literal summary')")
        .bind(slug).execute(&pool).await?;
    sqlx::query("INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('alias', $1)")
        .bind(slug)
        .execute(&pool)
        .await?;
    let literal = "---\ntitle: Literal title\nslug: literal\n---\n\n## Summary\n\nliteral file\n";
    std::fs::write(directory.join(format!("{slug}.md")), literal)?;
    let server = mcp::Server::new(
        Arc::new(
            memory_postgres::PostgresMemory::new(pool)
                .with_wiki_directory(root.path().to_path_buf()),
        ),
        false,
        CancellationToken::new(),
    )?;
    let mut request = CallToolRequestParams::new("wiki_read");
    request.arguments = Some(serde_json::from_str(r#"{"slug":"alias"}"#)?);
    let result = server.call(request).await?;
    assert_eq!(result.is_error, None);
    assert_eq!(
        result
            .content
            .first()
            .ok_or("content")?
            .as_text()
            .ok_or("text")?
            .text,
        literal
    );
    for arguments in [
        r#"{"slug":"../topics-private/secret"}"#,
        r#"{"slug":"../topics-private/../topics-private/secret"}"#,
    ] {
        let mut request = CallToolRequestParams::new("wiki_read");
        request.arguments = Some(serde_json::from_str(arguments)?);
        assert_eq!(server.call(request).await?.is_error, Some(true));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            sibling.join("secret.md"),
            directory.join("kiwi-build-cache.md"),
        )?;
        let mut request = CallToolRequestParams::new("wiki_read");
        request.arguments = Some(serde_json::from_str(r#"{"slug":"kiwi-build-cache"}"#)?);
        let result = server.call(request).await?;
        assert_eq!(result.is_error, Some(true));
        assert!(
            !result
                .content
                .first()
                .ok_or("content")?
                .as_text()
                .ok_or("text")?
                .text
                .contains("private")
        );
    }
    Ok(())
}

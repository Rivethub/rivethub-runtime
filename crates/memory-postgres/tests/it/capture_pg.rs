#[path = "capture_pg/hook.rs"]
mod hook;
#[path = "capture_pg/transcript.rs"]
mod transcript;
use hook::ingest_hook_event;
use transcript::ingest_transcript;

#[path = "capture_pg/deviations.rs"]
mod deviations;

use serde_json::Value;
use sqlx::PgPool;
use testkit::Json;
use testkit::capture_pg;
use testkit::capture_pg::PgOutcome;
use testkit::pgdump::dump_rivethub;
use testkit::pgdump::queue_present;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GOLDEN_FILES: [&str; 5] = [
    "transcript.json",
    "task.json",
    "routed.json",
    "hook.json",
    "errors.json",
];

fn at<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

async fn reset(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("truncate rh_conversations cascade")
        .execute(pool)
        .await?;
    sqlx::query("delete from rh_tags").execute(pool).await?;
    sqlx::query("delete from rh_tag_taxonomy")
        .execute(pool)
        .await?;
    if queue_present(pool).await? {
        sqlx::query("delete from graphile_worker._private_jobs")
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[sqlx::test]
async fn replay_public_arm(
    _options: sqlx::postgres::PgPoolOptions,
    connect: sqlx::postgres::PgConnectOptions,
) -> TestResult {
    let name = ephemeral_name()?;
    let admin = PgPool::connect_with(connect.clone().database("postgres")).await?;
    create_database(&admin, &name).await?;
    let result = replay_in(&connect, &name).await;
    let dropped = remove_database(&admin, &name).await;
    admin.close().await;
    result?;
    dropped
}

fn ephemeral_name() -> Result<String, Box<dyn std::error::Error>> {
    let name = format!("rr_replay_{}", uuid::Uuid::new_v4().simple());
    quoted_catalog(&name)?;
    Ok(name)
}

fn quoted_catalog(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    if name
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        Ok(format!("\"{name}\""))
    } else {
        Err("ephemeral database name".into())
    }
}

async fn create_database(admin: &PgPool, name: &str) -> TestResult {
    let quoted = quoted_catalog(name)?;
    execute_audited(admin, format!("CREATE DATABASE {quoted}")).await
}

async fn remove_database(admin: &PgPool, name: &str) -> TestResult {
    let quoted = quoted_catalog(name)?;
    execute_audited(
        admin,
        format!("DROP DATABASE IF EXISTS {quoted} WITH (FORCE)"),
    )
    .await
}

async fn execute_audited(admin: &PgPool, sql: String) -> TestResult {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(admin)
        .await?;
    Ok(())
}

async fn replay_in(connect: &sqlx::postgres::PgConnectOptions, name: &str) -> TestResult {
    let pool = PgPool::connect_with(connect.clone().database(name)).await?;
    let result = migrated_replay(&pool).await;
    pool.close().await;
    result
}

async fn migrated_replay(pool: &PgPool) -> TestResult {
    migrate::POSTGRES.run(pool).await?;
    replay_recorded(pool).await
}

async fn replay_recorded(pool: &PgPool) -> TestResult {
    let queue = queue_present(pool).await?;
    let skipped = deviations::exclusions()?;
    let mut failures = Vec::new();
    for file in GOLDEN_FILES {
        let path = testkit::recording(&format!("capture/pg/{file}"));
        for scenario in capture_pg::scenarios(&path)? {
            if skipped.contains_key(&format!("{file}/{}/{}", scenario.name, scenario.schema)) {
                continue;
            }
            let mut scenario = scenario.clone();
            if !queue {
                drop_graphile_rows(&mut scenario);
            }
            reset(pool).await?;
            let started = chrono::Utc::now() - chrono::Duration::seconds(5);
            let mut outcomes: Vec<PgOutcome> = Vec::new();
            let mut namer = testkit::PgNamer::new();
            for step in &scenario.steps {
                let call = capture_pg::PgStep {
                    kind: step.kind.clone(),
                    label: step.label.clone(),
                    input: step.input.clone(),
                };
                let mut context = StepContext {
                    pool,
                    started,
                    namer: &mut namer,
                };
                let mut outcome = run_step(&call, &mut context).await;
                if let Some(object) = outcome.result.as_mut().and_then(Json::as_object_mut)
                    && let Some(id) = object
                        .get("conversationId")
                        .and_then(Value::as_str)
                        .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
                {
                    let placeholder = namer.conv(&id.to_string());
                    object.insert("conversationId".to_string(), Value::String(placeholder));
                }
                if !queue
                    && let Some(rows) = outcome.rows.as_mut()
                    && let Some(object) = rows.as_object_mut()
                {
                    object.remove("graphile_jobs");
                }
                outcomes.push(outcome);
            }
            let mut remaining = outcomes.into_iter();
            let replay =
                |_step: &capture_pg::PgStep| -> PgOutcome { remaining.next().unwrap_or_default() };
            if let Err(mismatch) = capture_pg::replay_scenario(&scenario, None, replay) {
                failures.push(format!("{}: {}", mismatch.id, mismatch.detail));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n").into())
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn scenario_inventory_is_covered(_pool: PgPool) -> TestResult {
    let mut total = 0usize;
    let mut seen: Vec<String> = Vec::new();
    let skipped = deviations::exclusions()?;
    for file in GOLDEN_FILES {
        let path = testkit::recording(&format!("capture/pg/{file}"));
        for scenario in capture_pg::scenarios(&path)? {
            seen.push(format!("{file}/{}/{}", scenario.name, scenario.schema));
            total += 1;
        }
    }
    assert_eq!(total, 30, "recorded scenarios: {seen:?}");
    assert_eq!(skipped.len(), 8);
    for (name, reason) in skipped {
        assert_ne!(reason, "");
        assert!(seen.contains(&name), "unknown exclusion: {name}");
    }
    Ok(())
}

fn drop_graphile_rows(scenario: &mut capture_pg::ScenarioRecord) {
    for step in &mut scenario.steps {
        if let Some(rows) = step.rows.as_mut()
            && let Some(object) = rows.as_object_mut()
        {
            object.remove("graphile_jobs");
        }
    }
}

struct StepContext<'a> {
    pool: &'a PgPool,
    started: chrono::DateTime<chrono::Utc>,
    namer: &'a mut testkit::PgNamer,
}

async fn run_step(step: &capture_pg::PgStep, context: &mut StepContext<'_>) -> PgOutcome {
    let mut outcome = PgOutcome::default();
    match step.kind.as_str() {
        "sql" => {
            if step.label == "another writer creates the conversation without task_id"
                && let Err(error) = sqlx::query(
                    "INSERT INTO rh_conversations (session_key, agent, owner_user_id) VALUES ('claude-code:spawn-three', 'rivet-claude', 'owner')",
                )
                .execute(context.pool)
                .await
            {
                outcome.error = Some(error.to_string());
            }
        }
        "ingestHookEvent" => {
            let payload = at(&step.input, "payload");
            match ingest_hook_event(context.pool, payload, &step.input).await {
                Ok(result) => outcome.result = Some(result),
                Err(_) => outcome.error = Some("error".to_string()),
            }
        }
        "ingestTranscript" => {
            let path = at(&step.input, "transcriptPath").as_str().unwrap_or_default();
            let lines = at(&step.input, "transcriptLines").as_array();
            match ingest_transcript(context.pool, path, lines, at(&step.input, "options")).await {
                Ok(result) => outcome.result = Some(result),
                Err(_) => outcome.error = Some("error".to_string()),
            }
        }
        _ => {}
    }
    outcome.rows = Some(
        match dump_rivethub(context.pool, context.started, context.namer).await {
            Ok(rows) => rows,
            Err(mismatch) => encoded(DumpError {
                dump_error: format!("{mismatch}"),
            }),
        },
    );
    outcome
}

#[derive(Debug)]
struct IngestError;

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ingest replay failed")
    }
}

impl std::error::Error for IngestError {}

impl From<sqlx::Error> for IngestError {
    fn from(_: sqlx::Error) -> Self {
        Self
    }
}

impl From<memory_postgres::Error> for IngestError {
    fn from(_: memory_postgres::Error) -> Self {
        Self
    }
}

impl From<serde_json::Error> for IngestError {
    fn from(_: serde_json::Error) -> Self {
        Self
    }
}

const AGENT: &str = "rivet-claude";
const CHANNEL: &str = "claude-code";

fn str_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn encoded<T: serde::Serialize>(value: T) -> Json {
    serde_json::to_value(value).unwrap_or_default()
}

#[derive(serde::Serialize)]
struct HookOutcomeBody {
    #[serde(rename = "sessionKey")]
    session_key: String,
    #[serde(rename = "conversationId")]
    conversation_id: String,
    created: bool,
    inserted: u64,
    #[serde(rename = "alreadyStored", skip_serializing_if = "Option::is_none")]
    already_stored: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<String>,
}

#[derive(serde::Serialize)]
struct HookSettings<'a> {
    source: &'a str,
    last_event: &'a str,
    last_ingest_at: String,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

#[derive(serde::Serialize)]
struct TranscriptSettings {
    source: &'static str,
    file: String,
    session_id: Option<String>,
    pr_url: Option<String>,
    last_event: Option<String>,
    last_ingest_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

#[derive(serde::Serialize)]
struct DumpError {
    dump_error: String,
}

#[derive(serde::Serialize)]
struct HookEventMetadata {
    source: &'static str,
    hook_event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ingest_key: Option<String>,
    #[serde(flatten)]
    herdr: HerdrMetadata,
}

#[derive(serde::Serialize)]
struct TranscriptMetadata {
    source: &'static str,
    uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sidechain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovered: Option<bool>,
    #[serde(flatten)]
    herdr: HerdrMetadata,
}

#[derive(serde::Serialize, Default, Clone)]
struct HerdrMetadata {
    #[serde(rename = "herdr_pane_id", skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(rename = "herdr_workspace_id", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    #[serde(rename = "herdr_host", skip_serializing_if = "Option::is_none")]
    host: Option<String>,
}

impl HerdrMetadata {
    fn from_opts(opts: &Value) -> Self {
        opts.get("herdr").map_or_else(Self::default, |herdr| Self {
            pane_id: str_at(herdr, "paneId"),
            workspace_id: str_at(herdr, "workspaceId"),
            host: str_at(herdr, "host"),
        })
    }
}

impl TranscriptMetadata {
    fn build(
        uuid: Option<&str>,
        sidechain: Option<bool>,
        hook_event: Option<&str>,
        recovered: Option<bool>,
        herdr: HerdrMetadata,
    ) -> Result<wire::JsonObject, IngestError> {
        Ok(serde_json::from_value(encoded(Self {
            source: "claude-code-hook",
            uuid: uuid.map(str::to_string),
            sidechain,
            hook_event: hook_event.map(str::to_string),
            recovered,
            herdr,
        }))?)
    }
}

fn parse_stamp(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

fn routed_user(opts: &Value) -> String {
    opts.get("env")
        .and_then(|env| str_at(env, "RIVETOS_USER_ID"))
        .map(|raw| raw.trim().to_string())
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or_else(|| testkit::OWNER.to_owned())
}

fn task_id(opts: &Value) -> Option<String> {
    let raw = str_at(opts, "taskId")?;
    uuid::Uuid::parse_str(&raw).ok().map(|_| raw)
}

fn path_key(path: &str) -> String {
    let rel = path.strip_prefix("__FIXTURES__/").map_or_else(
        || path.to_string(),
        |rest| format!("__FIXTURES_REL__/{rest}"),
    );
    let rel = rel.strip_prefix('/').unwrap_or(&rel).to_string();
    let trimmed = rel.strip_suffix(".jsonl").unwrap_or(&rel).to_string();
    format!("claude-code:{trimmed}")
}

fn text_parts(content: &Value) -> (Option<String>, Vec<(String, String, Value)>) {
    let mut segments: Vec<String> = Vec::new();
    let mut calls: Vec<(String, String, Value)> = Vec::new();
    match content {
        Value::String(text) => {
            if !text.is_empty() {
                segments.push(text.clone());
            }
        }
        Value::Array(parts) => {
            for part in parts {
                match str_at(part, "type").unwrap_or_default().as_str() {
                    "text" => {
                        if let Some(text) = str_at(part, "text") {
                            segments.push(text);
                        }
                    }
                    "thinking" => {
                        if let Some(text) = str_at(part, "thinking") {
                            segments.push(format!("[thinking] {text}"));
                        }
                    }
                    "tool_use" => {
                        let id = str_at(part, "id").unwrap_or_default();
                        let name = str_at(part, "name").unwrap_or_default();
                        let input = part.get("input").cloned().unwrap_or(Value::Null);
                        calls.push((id, name, input));
                    }
                    _ => {}
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {}
    }
    let text = if segments.is_empty() {
        None
    } else {
        Some(segments.join("\n"))
    };
    (text, calls)
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut segments: Vec<String> = Vec::new();
            for part in parts {
                match part {
                    Value::String(text) => segments.push(text.clone()),
                    Value::Object(object) => {
                        if str_at(part, "type").as_deref() == Some("text") {
                            if let Some(text) = str_at(part, "text") {
                                segments.push(text);
                            }
                        } else if let Some(text) = object.get("content").and_then(Value::as_str) {
                            segments.push(text.to_string());
                        }
                    }
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => {}
                }
            }
            segments.join("\n")
        }
        other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_)) => {
            other.to_string()
        }
    }
}

const CONTENT_CAP: usize = 16_000;
const TITLE_CAP: usize = 120;
const TRUNCATION_MARKER: &str = "\n\u{2026}[truncated]";

fn capped(text: &str) -> String {
    match text.char_indices().nth(CONTENT_CAP) {
        Some((end, _)) => format!("{}{TRUNCATION_MARKER}", &text[..end]),
        None => text.to_owned(),
    }
}

fn title_of(text: &str) -> String {
    text.chars().take(TITLE_CAP).collect()
}

fn result_object(value: &Value) -> Result<Option<wire::JsonObject>, IngestError> {
    Ok(if value.is_object() {
        Some(serde_json::from_value(value.clone())?)
    } else {
        None
    })
}

fn tool_result_of(payload: &Value) -> Option<String> {
    let raw = match payload.get("tool_response") {
        None | Some(Value::Null) => str_at(payload, "tool_result")?,
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    Some(capped(&raw))
}

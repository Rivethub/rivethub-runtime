use chrono::DateTime;
use chrono::Utc;
use regex::Regex;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::Row as _;
use sqlx::Transaction;
use sqlx::postgres::Postgres;

use crate::Error;
use crate::Json;

pub(crate) const PG_TS: &str = r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(\.\d+)?\+00$";
pub(crate) const ISO_TS: &str = r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})";

const UUID_PATTERN: &str =
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";

const CONVERSATION_SELECT: &str = "SELECT id::text, session_key::text, agent::text, channel::text, channel_id::text, \
     bot_identity::text, title::text, settings::text, active::text, created_at::text, \
     updated_at::text, task_id::text, owner_user_id::text \
     FROM rh_conversations ORDER BY session_key, agent";

const MESSAGE_SELECT: &str = "SELECT id::text, conversation_id::text, agent::text, channel::text, role::text, \
     content::text, tool_name::text, tool_args::text, tool_result::text, metadata::text, \
     access_count::text, last_accessed_at::text, created_at::text, embed_failures::text, \
     embed_error::text, embed_status::text, owner_user_id::text, content_hash::text \
     FROM rh_messages ORDER BY ctid";

pub async fn queue_present(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let row =
        sqlx::query("select to_regclass('graphile_worker._private_jobs') is not null as present")
            .fetch_one(pool)
            .await?;
    row.try_get::<bool, _>("present")
}

pub async fn capture_tx(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mode = if queue_present(pool).await? {
        "off"
    } else {
        "on"
    };
    sqlx::query("select set_config('rivet.defer_embed_enqueue', $1, true) as mode")
        .bind(mode)
        .fetch_one(&mut *tx)
        .await?;
    Ok(tx)
}

pub async fn dump_rivethub(
    pool: &PgPool,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    let connection = pool.acquire().await.map_err(|error| dump_error(&error))?;
    let mut guard = connection;
    sqlx::raw_sql("SET TIME ZONE 'UTC'")
        .execute(&mut *guard)
        .await
        .map_err(|error| dump_error(&error))?;
    let mut rows = serde_json::Map::new();
    rows.insert(
        "ros_conversations".to_string(),
        dump_conversations(&mut guard, started, namer).await?,
    );
    rows.insert(
        "ros_messages".to_string(),
        dump_messages(&mut guard, started, namer).await?,
    );
    rows.insert(
        "graphile_jobs".to_string(),
        dump_jobs(pool, &mut guard, started, namer).await?,
    );
    let summaries = sqlx::query("SELECT count(*)::text AS n FROM rh_summaries")
        .fetch_one(&mut *guard)
        .await
        .map_err(|error| dump_error(&error))?;
    rows.insert(
        "ros_summaries_count".to_string(),
        number_text(
            &summaries
                .try_get::<String, _>("n")
                .map_err(|error| dump_error(&error))?,
        )?,
    );
    Ok(Value::Object(rows))
}

fn dump_error(source: &sqlx::Error) -> Error {
    Error::Format {
        path: std::path::PathBuf::from("pg replay"),
        message: source.to_string(),
    }
}

fn text_cell(
    raw: Option<String>,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    match raw {
        Some(raw) => namer.normalize(&raw, started),
        None => Ok(Json::Null),
    }
}

fn bool_cell(raw: Option<&str>) -> Result<Json, Error> {
    match raw {
        Some(raw) => Ok(Json::Bool(raw.parse::<bool>()?)),
        None => Ok(Json::Null),
    }
}

fn count_cell(raw: Option<String>) -> Result<Json, Error> {
    match raw {
        Some(raw) => number_text(&raw),
        None => Ok(Json::Null),
    }
}

fn id_cell(
    raw: Option<String>,
    kind: IdKind,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    match raw {
        Some(uuid) if looks_like_uuid(&uuid) => Ok(Json::String(match kind {
            IdKind::Conversation => namer.conv(&uuid),
            IdKind::Message => namer.msg(&uuid),
        })),
        raw => text_cell(raw, started, namer),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdKind {
    Conversation,
    Message,
}

async fn dump_conversations(
    guard: &mut sqlx::pool::PoolConnection<Postgres>,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    let fetched = sqlx::query(CONVERSATION_SELECT)
        .fetch_all(&mut **guard)
        .await
        .map_err(|error| dump_error(&error))?;
    let mut out = Vec::new();
    for row in fetched {
        let column = |index: usize| -> Result<Option<String>, Error> {
            row.try_get(index).map_err(|error| dump_error(&error))
        };
        out.push(serde_json::json!({
            "id": id_cell(column(0)?, IdKind::Conversation, started, namer)?,
            "session_key": text_cell(column(1)?, started, namer)?,
            "agent": text_cell(column(2)?, started, namer)?,
            "channel": text_cell(column(3)?, started, namer)?,
            "channel_id": text_cell(column(4)?, started, namer)?,
            "bot_identity": text_cell(column(5)?, started, namer)?,
            "title": text_cell(column(6)?, started, namer)?,
            "settings": text_cell(column(7)?, started, namer)?,
            "active": bool_cell(column(8)?.as_deref())?,
            "created_at": text_cell(column(9)?, started, namer)?,
            "updated_at": text_cell(column(10)?, started, namer)?,
            "task_id": text_cell(column(11)?, started, namer)?,
            "owner_user_id": text_cell(column(12)?, started, namer)?,
        }));
    }
    Ok(Json::Array(out))
}

async fn dump_messages(
    guard: &mut sqlx::pool::PoolConnection<Postgres>,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    let fetched = sqlx::query(MESSAGE_SELECT)
        .fetch_all(&mut **guard)
        .await
        .map_err(|error| dump_error(&error))?;
    let mut out = Vec::new();
    for row in fetched {
        let column = |index: usize| -> Result<Option<String>, Error> {
            row.try_get(index).map_err(|error| dump_error(&error))
        };
        out.push(serde_json::json!({
            "id": id_cell(column(0)?, IdKind::Message, started, namer)?,
            "conversation_id": id_cell(column(1)?, IdKind::Conversation, started, namer)?,
            "agent": text_cell(column(2)?, started, namer)?,
            "channel": text_cell(column(3)?, started, namer)?,
            "role": text_cell(column(4)?, started, namer)?,
            "content": text_cell(column(5)?, started, namer)?,
            "tool_name": text_cell(column(6)?, started, namer)?,
            "tool_args": text_cell(column(7)?, started, namer)?,
            "tool_result": text_cell(column(8)?, started, namer)?,
            "metadata": text_cell(column(9)?, started, namer)?,
            "access_count": count_cell(column(10)?)?,
            "last_accessed_at": text_cell(column(11)?, started, namer)?,
            "created_at": text_cell(column(12)?, started, namer)?,
            "embed_failures": count_cell(column(13)?)?,
            "embed_error": text_cell(column(14)?, started, namer)?,
            "embed_status": text_cell(column(15)?, started, namer)?,
            "owner_user_id": text_cell(column(16)?, started, namer)?,
            "content_hash": text_cell(column(17)?, started, namer)?,
        }));
    }
    Ok(Json::Array(out))
}

async fn dump_jobs(
    pool: &PgPool,
    guard: &mut sqlx::pool::PoolConnection<Postgres>,
    started: DateTime<Utc>,
    namer: &mut PgNamer,
) -> Result<Json, Error> {
    if !queue_present(pool)
        .await
        .map_err(|error| dump_error(&error))?
    {
        return Ok(Json::Array(Vec::new()));
    }
    let fetched = sqlx::query(
        "SELECT t.identifier AS task, q.queue_name AS queue, j.payload::text AS payload, \
                j.key::text AS key, j.priority::text AS priority, \
                j.max_attempts::text AS max_attempts, j.attempts::text AS attempts \
           FROM graphile_worker._private_jobs j \
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id \
           LEFT JOIN graphile_worker._private_job_queues q ON q.id = j.job_queue_id \
          ORDER BY j.id",
    )
    .fetch_all(&mut **guard)
    .await
    .map_err(|error| dump_error(&error))?;
    let mut jobs = Vec::new();
    for row in fetched {
        let column = |name: &'static str| -> Result<String, Error> {
            row.try_get(name).map_err(|error| dump_error(&error))
        };
        let nullable_column = |name: &'static str| -> Result<Option<String>, Error> {
            row.try_get(name).map_err(|error| dump_error(&error))
        };
        jobs.push(serde_json::json!({
            "task": column("task")?,
            "queue": nullable_column("queue")?,
            "payload": namer.rewrite_embedded(&column("payload")?, started)?,
            "key": nullable_column("key")?.map(|k| namer.rewrite_embedded(&k, started)).transpose()?,
            "priority": number_text(&column("priority")?)?,
            "max_attempts": number_text(&column("max_attempts")?)?,
            "attempts": number_text(&column("attempts")?)?,
        }));
    }
    Ok(Json::Array(jobs))
}

fn number_text(raw: &str) -> Result<Json, Error> {
    Ok(Json::from(raw.parse::<i64>()?))
}

fn looks_like_uuid(text: &str) -> bool {
    text.len() == 36 && uuid::Uuid::try_parse(text).is_ok()
}

fn parse_timestamp(text: &str, started: DateTime<Utc>) -> Result<bool, Error> {
    let parsed = if text.ends_with("+00") {
        chrono::NaiveDateTime::parse_from_str(text.trim_end_matches("+00"), "%Y-%m-%d %H:%M:%S%.f")?
            .and_utc()
    } else {
        DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc)
    };
    Ok(parsed >= started)
}

fn timestamp_text(text: &str, started: DateTime<Utc>) -> Result<bool, Error> {
    if Regex::new(PG_TS)?.is_match(text) || Regex::new(&format!("^{ISO_TS}$"))?.is_match(text) {
        parse_timestamp(text, started)
    } else {
        Ok(false)
    }
}

#[derive(Debug, Default)]
pub struct PgNamer {
    conv: Vec<String>,
    msg: Vec<String>,
    ids: Vec<String>,
    now: Vec<String>,
}

impl PgNamer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            conv: Vec::new(),
            msg: Vec::new(),
            ids: Vec::new(),
            now: Vec::new(),
        }
    }

    fn assign(bucket: &mut Vec<String>, text: &str, prefix: &str) -> String {
        if let Some(index) = bucket.iter().position(|seen| seen == text) {
            return format!("{prefix}-{}", index + 1);
        }
        bucket.push(text.to_string());
        format!("{prefix}-{}", bucket.len())
    }

    pub fn conv(&mut self, uuid: &str) -> String {
        Self::assign(&mut self.conv, uuid, "conv")
    }

    pub fn msg(&mut self, uuid: &str) -> String {
        Self::assign(&mut self.msg, uuid, "msg")
    }

    fn any(&mut self, uuid: &str) -> String {
        if let Some(index) = self.conv.iter().position(|seen| seen == uuid) {
            return format!("conv-{}", index + 1);
        }
        if let Some(index) = self.msg.iter().position(|seen| seen == uuid) {
            return format!("msg-{}", index + 1);
        }
        Self::assign(&mut self.ids, uuid, "id")
    }

    fn now(&mut self, text: &str) -> String {
        if let Some(index) = self.now.iter().position(|seen| seen == text) {
            return format!("<now-{}>", index + 1);
        }
        self.now.push(text.to_string());
        format!("<now-{}>", self.now.len())
    }

    pub fn normalize(&mut self, text: &str, started: DateTime<Utc>) -> Result<Json, Error> {
        if timestamp_text(text, started)? {
            return Ok(Json::String(self.now(text)));
        }
        let mut replaced = text.to_string();
        for found in Regex::new(ISO_TS)?.find_iter(text) {
            let candidate = found.as_str().to_string();
            if parse_timestamp(&candidate, started)? {
                let placeholder = self.now(&candidate);
                replaced = replaced.replace(&candidate, &placeholder);
            }
        }
        Ok(Json::String(replaced))
    }

    pub fn rewrite_embedded(&mut self, text: &str, started: DateTime<Utc>) -> Result<Json, Error> {
        let stamped = self.normalize(text, started)?;
        let Json::String(stamped) = stamped else {
            return Ok(stamped);
        };
        let mut out = stamped.clone();
        for found in Regex::new(UUID_PATTERN)?.find_iter(&stamped) {
            let candidate = found.as_str().to_string();
            let placeholder = self.any(&candidate);
            out = out.replace(&candidate, &placeholder);
        }
        Ok(Json::String(out))
    }
}

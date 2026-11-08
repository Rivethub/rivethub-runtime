use std::str::FromStr;

use async_trait::async_trait;
use kernel::memory::MemoryStoreError;
use kernel::model_calls::{ModelCall, ModelCallStore, RecordedCalls, TurnMetadata};
use kernel::owner::Owner;
use rusqlite::{Connection, params};
use wire::model_call::{CallOutcome, CallUsage};
use wire::session_id::SessionId;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const RECORD: &str = "model call record";
const READ: &str = "model call read";

fn signed(value: u64) -> Result<i64, SqliteError> {
    i64::try_from(value).map_err(|_| SqliteError::Invalid {
        message: "a model call count does not fit a signed 64-bit integer".to_owned(),
    })
}

fn unsigned(value: i64) -> Result<u64, SqliteError> {
    u64::try_from(value).map_err(SqliteError::Count)
}

fn record(
    conn: &mut Connection,
    owner: &str,
    calls: &[ModelCall],
) -> Result<RecordedCalls, SqliteError> {
    let transaction = conn.transaction().map_err(SqliteError::query(RECORD))?;
    let mut inserted = 0_u64;
    for call in calls {
        let usage = call.usage;
        let turn = call.turn.as_ref();
        let written = transaction
            .execute(
                "INSERT INTO rh_model_calls (call_id, owner_user_id, session_key, turn_id, request_kind,
                    started_at, finished_at, provider, model_requested, model_served, account,
                    status_code, outcome, retry_after_secs, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, reasoning_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT (call_id) DO NOTHING",
                params![
                    call.call_id,
                    owner,
                    call.session.as_ref().map(SessionId::as_str),
                    turn.and_then(|turn| turn.turn_id.as_deref()),
                    turn.and_then(|turn| turn.request_kind.as_deref()),
                    time::stamp(call.started_at),
                    call.finished_at.map(time::stamp),
                    call.provider,
                    call.model_requested,
                    call.model_served,
                    call.account,
                    call.status_code,
                    call.outcome.to_string(),
                    call.retry_after_secs.map(signed).transpose()?,
                    usage.map(|usage| signed(usage.input)).transpose()?,
                    usage.map(|usage| signed(usage.output)).transpose()?,
                    usage.map(|usage| signed(usage.cache_read)).transpose()?,
                    usage.map(|usage| signed(usage.cache_write)).transpose()?,
                    usage.map(|usage| signed(usage.reasoning)).transpose()?,
                ],
            )
            .map_err(SqliteError::query(RECORD))?;
        inserted += u64::try_from(written).map_err(SqliteError::Count)?;
    }
    transaction.commit().map_err(SqliteError::query(RECORD))?;
    let total = u64::try_from(calls.len()).map_err(SqliteError::Count)?;
    Ok(RecordedCalls {
        inserted,
        duplicate: total - inserted,
    })
}

struct Row {
    call_id: String,
    session_key: Option<String>,
    turn_id: Option<String>,
    request_kind: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    provider: String,
    model_requested: Option<String>,
    model_served: Option<String>,
    account: Option<String>,
    status_code: Option<u16>,
    outcome: String,
    retry_after_secs: Option<i64>,
    tokens: [Option<i64>; 5],
}

impl Row {
    fn into_call(self) -> Result<ModelCall, SqliteError> {
        let [input, output, cache_read, cache_write, reasoning] = self.tokens;
        let usage = match input {
            Some(input) => Some(CallUsage {
                input: unsigned(input)?,
                output: unsigned(output.unwrap_or_default())?,
                cache_read: unsigned(cache_read.unwrap_or_default())?,
                cache_write: unsigned(cache_write.unwrap_or_default())?,
                reasoning: unsigned(reasoning.unwrap_or_default())?,
            }),
            None => None,
        };
        let turn = TurnMetadata::of(self.turn_id, self.request_kind);
        Ok(ModelCall {
            call_id: self.call_id,
            started_at: time::stored(&self.started_at, "rh_model_calls.started_at")?,
            finished_at: self
                .finished_at
                .as_deref()
                .map(|text| time::stored(text, "rh_model_calls.finished_at"))
                .transpose()?,
            provider: self.provider,
            model_requested: self.model_requested,
            model_served: self.model_served,
            account: self.account,
            status_code: self.status_code,
            outcome: CallOutcome::from_str(&self.outcome).map_err(|_| {
                SqliteError::StoredValue {
                    column: "rh_model_calls.outcome",
                    value: self.outcome,
                }
            })?,
            retry_after_secs: self.retry_after_secs.map(unsigned).transpose()?,
            usage,
            session: self
                .session_key
                .map(|key| {
                    SessionId::from_str(&key).map_err(|_| SqliteError::StoredValue {
                        column: "rh_model_calls.session_key",
                        value: key,
                    })
                })
                .transpose()?,
            turn,
        })
    }
}

fn session_calls(conn: &Connection, session: &str) -> Result<Vec<ModelCall>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT call_id, session_key, turn_id, request_kind, started_at, finished_at, provider,
                    model_requested, model_served, account, status_code, outcome, retry_after_secs,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens
               FROM rh_model_calls WHERE session_key = ?1
              ORDER BY started_at, call_id",
        )
        .map_err(SqliteError::query(READ))?;
    let rows = statement
        .query_map([session], |row| {
            Ok(Row {
                call_id: row.get(0)?,
                session_key: row.get(1)?,
                turn_id: row.get(2)?,
                request_kind: row.get(3)?,
                started_at: row.get(4)?,
                finished_at: row.get(5)?,
                provider: row.get(6)?,
                model_requested: row.get(7)?,
                model_served: row.get(8)?,
                account: row.get(9)?,
                status_code: row.get(10)?,
                outcome: row.get(11)?,
                retry_after_secs: row.get(12)?,
                tokens: [
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                ],
            })
        })
        .map_err(SqliteError::query(READ))?;
    let mut calls = Vec::new();
    for row in rows {
        calls.push(row.map_err(SqliteError::query(READ))?.into_call()?);
    }
    Ok(calls)
}

#[async_trait]
impl ModelCallStore for SqliteStore {
    async fn record(
        &self,
        owner: &Owner,
        calls: &[ModelCall],
    ) -> Result<RecordedCalls, MemoryStoreError> {
        let owner = owner.as_str().to_owned();
        let calls = calls.to_vec();
        self.with_conn(move |conn| record(conn, &owner, &calls))
            .await
            .map_err(Into::into)
    }

    async fn session_calls(&self, session: &SessionId) -> Result<Vec<ModelCall>, MemoryStoreError> {
        let session = session.as_str().to_owned();
        self.with_conn(move |conn| session_calls(conn, &session))
            .await
            .map_err(Into::into)
    }
}

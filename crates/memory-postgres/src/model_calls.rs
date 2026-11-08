use std::str::FromStr;

use async_trait::async_trait;
use kernel::memory::MemoryStoreError;
use kernel::model_calls::{ModelCall, ModelCallStore, RecordedCalls, TurnMetadata};
use kernel::owner::Owner;
use wire::model_call::{CallOutcome, CallUsage};
use wire::session_id::SessionId;

use crate::PostgresMemory;
use crate::error::Error;

fn signed(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(Error::from)
}

fn unsigned(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(Error::from)
}

struct Row {
    call_id: String,
    session_key: Option<String>,
    turn_id: Option<String>,
    request_kind: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    provider: String,
    model_requested: Option<String>,
    model_served: Option<String>,
    account: Option<String>,
    status_code: Option<i32>,
    outcome: String,
    retry_after_secs: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
}

impl Row {
    fn into_call(self) -> Result<ModelCall, Error> {
        let usage = match self.input_tokens {
            Some(input) => Some(CallUsage {
                input: unsigned(input)?,
                output: unsigned(self.output_tokens.unwrap_or_default())?,
                cache_read: unsigned(self.cache_read_tokens.unwrap_or_default())?,
                cache_write: unsigned(self.cache_write_tokens.unwrap_or_default())?,
                reasoning: unsigned(self.reasoning_tokens.unwrap_or_default())?,
            }),
            None => None,
        };
        let turn = (self.turn_id.is_some() || self.request_kind.is_some()).then(|| TurnMetadata {
            turn_id: self.turn_id,
            request_kind: self.request_kind,
        });
        Ok(ModelCall {
            call_id: self.call_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            provider: self.provider,
            model_requested: self.model_requested,
            model_served: self.model_served,
            account: self.account,
            status_code: self
                .status_code
                .map(u16::try_from)
                .transpose()
                .map_err(Error::from)?,
            outcome: CallOutcome::from_str(&self.outcome)
                .map_err(|source| Error::stored("model call outcome", source))?,
            retry_after_secs: self.retry_after_secs.map(unsigned).transpose()?,
            usage,
            session: self
                .session_key
                .as_deref()
                .map(SessionId::from_str)
                .transpose()
                .map_err(|source| Error::stored("model call session key", source))?,
            turn,
        })
    }
}

#[async_trait]
impl ModelCallStore for PostgresMemory {
    async fn record(
        &self,
        owner: &Owner,
        calls: &[ModelCall],
    ) -> Result<RecordedCalls, MemoryStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| Error::operation("model call record", source))?;
        let mut inserted = 0_u64;
        for call in calls {
            let usage = call.usage;
            let written = sqlx::query!(
                "INSERT INTO rh_model_calls (call_id, owner_user_id, session_key, turn_id, request_kind,
                    started_at, finished_at, provider, model_requested, model_served, account,
                    status_code, outcome, retry_after_secs, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, reasoning_tokens)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
                 ON CONFLICT (call_id) DO NOTHING",
                call.call_id,
                owner.as_str(),
                call.session.as_ref().map(SessionId::as_str),
                call.turn.as_ref().and_then(|turn| turn.turn_id.as_deref()),
                call.turn.as_ref().and_then(|turn| turn.request_kind.as_deref()),
                call.started_at,
                call.finished_at,
                call.provider,
                call.model_requested,
                call.model_served,
                call.account,
                call.status_code.map(i32::from),
                call.outcome.to_string(),
                call.retry_after_secs.map(signed).transpose()?,
                usage.map(|usage| signed(usage.input)).transpose()?,
                usage.map(|usage| signed(usage.output)).transpose()?,
                usage.map(|usage| signed(usage.cache_read)).transpose()?,
                usage.map(|usage| signed(usage.cache_write)).transpose()?,
                usage.map(|usage| signed(usage.reasoning)).transpose()?
            )
            .execute(&mut *transaction)
            .await
            .map_err(|source| Error::operation("model call record", source))?;
            inserted += written.rows_affected();
        }
        transaction
            .commit()
            .await
            .map_err(|source| Error::operation("model call record", source))?;
        let total = u64::try_from(calls.len()).map_err(Error::from)?;
        Ok(RecordedCalls {
            inserted,
            duplicate: total - inserted,
        })
    }

    async fn session_calls(&self, session: &SessionId) -> Result<Vec<ModelCall>, MemoryStoreError> {
        let rows = sqlx::query_as!(
            Row,
            "SELECT call_id, session_key, turn_id, request_kind, started_at, finished_at, provider,
                    model_requested, model_served, account, status_code, outcome, retry_after_secs,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    reasoning_tokens
               FROM rh_model_calls WHERE session_key = $1
              ORDER BY started_at, call_id",
            session.as_str()
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("model call read", source))?;
        let calls = rows
            .into_iter()
            .map(Row::into_call)
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(calls)
    }
}

use crate::error::Error;
use chrono::DateTime;
use chrono::SubsecRound as _;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::owner::Owner;
use sqlx::PgPool;
use sqlx::Transaction;
use sqlx::postgres::Postgres;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

pub const MAX_CONTENT_CHARS: usize = 16_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    pub owner: Owner,
    pub node: Option<String>,
}

impl CaptureOptions {
    #[must_use]
    pub fn owned_by(owner: &Owner) -> Self {
        Self {
            owner: owner.clone(),
            node: None,
        }
    }
}

#[derive(Debug)]
pub struct CaptureWriter<'a> {
    pub(crate) queue_present: bool,
    pub(crate) tx: Transaction<'a, Postgres>,
    undated_from: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationUpsert<'a> {
    pub session_key: &'a str,
    pub agent: &'a str,
    pub channel: Option<&'a str>,
    pub title: Option<&'a str>,
    pub settings: Option<&'a JsonObject>,
    pub task_id: Option<&'a str>,
    pub owner_user_id: &'a str,
    pub node: Option<&'a str>,
    pub dates: ConversationDates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationDates {
    CaptureTime,
    Imported {
        created_at: Option<DateTime<Utc>>,
        updated_at: Option<DateTime<Utc>>,
    },
}

impl ConversationDates {
    #[must_use]
    pub fn of(batch: &CaptureBatchRequest) -> Self {
        if wire::capture::is_imported_session(&batch.session_key) {
            Self::Imported {
                created_at: batch.created_at,
                updated_at: batch.updated_at,
            }
        } else {
            Self::CaptureTime
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertedConversation {
    pub id: Uuid,
    pub owner_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow<'a> {
    pub conversation_id: Uuid,
    pub agent: &'a str,
    pub channel: &'a str,
    pub role: Role,
    pub content: &'a str,
    pub tool_name: Option<&'a str>,
    pub tool_args: Option<&'a JsonObject>,
    pub tool_result: Option<&'a str>,
    pub metadata: &'a JsonObject,
    pub owner_user_id: &'a str,
    pub created_at: Option<DateTime<Utc>>,
    pub batch_index: i64,
}

type PositionPatches<'a> = BTreeMap<&'a str, JsonObject>;

pub(crate) struct StagedMessages<'a> {
    pub(crate) fresh: Vec<(usize, &'a CaptureMessage)>,
    pub(crate) positions: PositionPatches<'a>,
}

impl<'a> CaptureWriter<'a> {
    pub async fn begin(pool: &'a PgPool, session_key: &str) -> Result<Self, Error> {
        let mut tx = pool
            .begin()
            .await
            .map_err(|source| Error::operation("capture begin", source))?;
        let _locked = sqlx::query!(
            "select pg_advisory_xact_lock(hashtext($1)) is not null as locked",
            session_key
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|source| Error::operation("capture lock", source))?;
        let undated_from = sqlx::query_scalar!(
            r#"select greatest(now(), max(last.created_at) + interval '1 microsecond') as "undated_from!"
                 from rh_conversations c
                 cross join lateral (
                     select m.created_at from rh_messages m
                      where m.conversation_id = c.id
                      order by m.created_at desc
                      limit 1
                 ) last
                where c.session_key = $1"#,
            session_key
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|source| Error::operation("capture start time", source))?;
        let queued = crate::graphile::GraphileQueue::schema_present(&mut *tx).await?;
        sqlx::query(
            "select set_config('rivet.defer_embed_enqueue', case when $1 then 'off' else 'on' end, true)",
        )
        .bind(queued)
        .execute(&mut *tx)
        .await
        .map_err(|source| Error::operation("embed defer", source))?;
        Ok(Self {
            tx,
            undated_from,
            queue_present: queued,
        })
    }

    pub(crate) async fn defer_embeddings(&mut self) -> Result<(), Error> {
        sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
            .execute(&mut *self.tx)
            .await
            .map_err(|source| Error::operation("embed defer", source))?;
        Ok(())
    }

    pub(crate) async fn carries_receipt(
        &mut self,
        session_key: &str,
        owner: &str,
    ) -> Result<bool, Error> {
        sqlx::query_scalar(
            "select exists (select 1 from rh_conversations
              where owner_user_id = $1 and session_key = $2 and settings ? 'receipt')",
        )
        .bind(owner)
        .bind(session_key)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("capture receipt check", source))
    }

    pub async fn commit(self) -> Result<(), Error> {
        self.tx
            .commit()
            .await
            .map_err(|source| Error::operation("capture commit", source))
    }

    pub async fn finalize_conversation(
        &mut self,
        id: Uuid,
        keeps_updated_at: bool,
    ) -> Result<(), Error> {
        sqlx::query!(
            "update rh_conversations
                set active = false,
                    updated_at = case when $2 then updated_at else now() end
              where id = $1 and active = true",
            id,
            keeps_updated_at
        )
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation finalize", source))?;
        Ok(())
    }

    pub async fn insert_message(&mut self, row: &MessageRow<'_>) -> Result<Uuid, Error> {
        let tool_args = row.tool_args.map(sqlx::types::Json);
        let metadata = sqlx::types::Json(row.metadata);
        let record = sqlx::query!(
            "insert into rh_messages
               (conversation_id, agent, channel, role, content, tool_name, tool_args,
                tool_result, metadata, owner_user_id, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                     coalesce($11, $13::timestamptz + $12::bigint * interval '1 microsecond'))
             returning id",
            row.conversation_id,
            row.agent,
            row.channel,
            row.role.to_string(),
            row.content,
            row.tool_name,
            tool_args as _,
            row.tool_result,
            metadata as _,
            row.owner_user_id,
            row.created_at,
            row.batch_index,
            self.undated_from,
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("message insert", source))?;
        Ok(record.id)
    }

    pub async fn imported_settings(
        &mut self,
        batch: &CaptureBatchRequest,
    ) -> Result<Option<wire::capture::ImportedSettings>, Error> {
        if !wire::capture::is_imported_session(&batch.session_key) {
            return Ok(None);
        }
        let stored = self
            .stored_settings(&batch.session_key, &batch.agent)
            .await?;
        Ok(Some(wire::capture::merge_imported_settings(
            stored.as_ref(),
            batch.settings.as_ref(),
        )?))
    }

    pub async fn untied_times(
        &mut self,
        conversation_id: Uuid,
        times: Vec<Option<DateTime<Utc>>>,
    ) -> Result<Vec<Option<DateTime<Utc>>>, Error> {
        let times: Vec<Option<DateTime<Utc>>> = times
            .into_iter()
            .map(|time| time.map(|at| at.trunc_subsecs(6)))
            .collect();
        let mut latest = BTreeMap::new();
        let mut asked = BTreeSet::new();
        let mut wanted = milliseconds(&times);
        loop {
            latest.extend(self.latest_by_millisecond(conversation_id, &wanted).await?);
            asked.extend(wanted);
            let placed = untie(&times, latest.clone());
            wanted = milliseconds(&placed)
                .into_iter()
                .filter(|millisecond| !asked.contains(millisecond))
                .collect();
            if wanted.is_empty() {
                return Ok(placed);
            }
        }
    }

    async fn latest_by_millisecond(
        &mut self,
        conversation_id: Uuid,
        milliseconds: &[DateTime<Utc>],
    ) -> Result<Vec<(DateTime<Utc>, DateTime<Utc>)>, Error> {
        if milliseconds.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query!(
            "select wanted.at as \"at!\",
                    (select max(m.created_at) from rh_messages m
                      where m.conversation_id = $1
                        and m.created_at >= wanted.at
                        and m.created_at < wanted.at + interval '1 millisecond') as latest
               from unnest($2::timestamptz[]) as wanted(at)",
            conversation_id,
            milliseconds
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("message time lookup", source))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.latest.map(|latest| (row.at, latest)))
            .collect())
    }

    async fn stored_settings(
        &mut self,
        session_key: &str,
        agent: &str,
    ) -> Result<Option<JsonObject>, Error> {
        let stored = sqlx::query_scalar!(
            "select settings as \"settings!: sqlx::types::Json<JsonObject>\"
               from rh_conversations where session_key = $1 and agent = $2",
            session_key,
            agent
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation settings lookup", source))?;
        Ok(stored.map(|settings| settings.0))
    }

    pub(crate) async fn stage_messages<'m>(
        &mut self,
        conversation_id: Uuid,
        messages: &'m [CaptureMessage],
    ) -> Result<StagedMessages<'m>, Error> {
        let event_ids: Vec<String> = messages
            .iter()
            .map(|message| message.event_id.clone())
            .collect();
        let seen = self.seen_event_ids(conversation_id, &event_ids).await?;
        let seen: HashSet<_> = seen.iter().map(String::as_str).collect();
        let mut fresh = Vec::new();
        let mut positions = PositionPatches::new();
        for (index, message) in messages.iter().enumerate() {
            if !seen.contains(message.event_id.as_str()) {
                fresh.push((index, message));
                continue;
            }
            wire::capture::add_transcript_position(
                positions.entry(message.event_id.as_str()).or_default(),
                message.metadata.as_ref(),
            )?;
        }
        positions.retain(|_, patch| !patch.as_map().is_empty());
        Ok(StagedMessages { fresh, positions })
    }

    pub(crate) async fn fill_positions(
        &mut self,
        conversation_id: Uuid,
        owner_user_id: &str,
        positions: &PositionPatches<'_>,
        compare_order: bool,
    ) -> Result<bool, Error> {
        let before = if compare_order {
            Some(crate::settlement::rendered_order(&mut self.tx, conversation_id).await?)
        } else {
            None
        };
        let result = sqlx::query(
            "update rh_messages m set metadata = replay.patch || m.metadata
               from jsonb_each($3) as replay(event_id, patch)
               cross join lateral (
                   select id from rh_messages
                    where conversation_id = $1 and owner_user_id = $2
                      and metadata->>'event_id' = replay.event_id
                    order by created_at, id limit 1
               ) as stored
              where m.id = stored.id and m.conversation_id = $1 and m.owner_user_id = $2
                and exists (
                    select 1 from jsonb_object_keys(replay.patch) as field(key)
                     where not (m.metadata ? field.key)
                )",
        )
        .bind(conversation_id)
        .bind(owner_user_id)
        .bind(sqlx::types::Json(positions))
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("transcript position fill", source))?;
        if let Some(before) = before
            && result.rows_affected() > 0
        {
            return Ok(
                before != crate::settlement::rendered_order(&mut self.tx, conversation_id).await?
            );
        }
        Ok(false)
    }

    pub async fn seen_event_ids(
        &mut self,
        conversation_id: Uuid,
        event_ids: &[String],
    ) -> Result<Vec<String>, Error> {
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }
        let seen = sqlx::query_scalar!(
            "select seen.event_id
               from unnest($2::text[]) as wanted(event_id)
              cross join lateral (
                  select m.metadata->>'event_id' as event_id from rh_messages m
                   where m.conversation_id = $1 and m.metadata->>'event_id' = wanted.event_id
                   limit 1
              ) seen",
            conversation_id,
            event_ids
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("event id lookup", source))?;
        Ok(seen.into_iter().flatten().collect())
    }
}

fn capped(text: &str) -> Result<(String, Option<u64>), Error> {
    let text = text.replace('\0', "");
    let kept = kernel::memory::budget::char_prefix(&text, MAX_CONTENT_CHARS);
    let full = (kept.len() < text.len())
        .then(|| u64::try_from(text.chars().count()))
        .transpose()?;
    Ok((kept.to_owned(), full))
}

pub(crate) struct PreparedMessage {
    pub(crate) content: String,
    pub(crate) tool_result: Option<String>,
    pub(crate) metadata: JsonObject,
}

fn milliseconds(times: &[Option<DateTime<Utc>>]) -> Vec<DateTime<Utc>> {
    times
        .iter()
        .flatten()
        .copied()
        .map(millisecond)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn untie(
    times: &[Option<DateTime<Utc>>],
    mut latest: BTreeMap<DateTime<Utc>, DateTime<Utc>>,
) -> Vec<Option<DateTime<Utc>>> {
    let tick = TimeDelta::microseconds(1);
    times
        .iter()
        .map(|time| {
            time.map(|at| {
                let mut placed = at;
                while let Some(&stored) = latest
                    .get(&millisecond(placed))
                    .filter(|&&stored| placed <= stored)
                {
                    placed = stored + tick;
                }
                latest.insert(millisecond(at), placed);
                latest.insert(millisecond(placed), placed);
                placed
            })
        })
        .collect()
}

fn millisecond(at: DateTime<Utc>) -> DateTime<Utc> {
    at - TimeDelta::nanoseconds(i64::from(at.timestamp_subsec_nanos() % 1_000_000))
}

pub(crate) fn prepare_capture_message(message: &CaptureMessage) -> Result<PreparedMessage, Error> {
    let (content, full_content_length) = capped(&message.content)?;
    let (tool_result, full_tool_result_length) = match message.tool_result.as_deref() {
        Some(text) => {
            let (kept, full) = capped(text)?;
            (Some(kept), full)
        }
        None => (None, None),
    };
    let mut metadata = message.metadata.clone();
    let metadata = metadata.get_or_insert_default();
    metadata.insert("event_id", &message.event_id)?;
    if let Some(length) = full_content_length {
        metadata.insert("truncated", true)?;
        metadata.insert("full_content_length", length)?;
    }
    if let Some(length) = full_tool_result_length {
        metadata.insert("full_tool_result_length", length)?;
    }
    Ok(PreparedMessage {
        content,
        tool_result,
        metadata: metadata.clone(),
    })
}

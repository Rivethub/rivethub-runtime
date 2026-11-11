use crate::capture::CaptureWriter;
use crate::capture::ConversationDates;
use crate::capture::ConversationEnsure;
use crate::capture::ConversationRecord;
use crate::capture::ConversationUpsert;
use crate::capture::UpsertedConversation;
use crate::error::Error;
use wire::JsonObject;

impl CaptureWriter<'_> {
    pub async fn ensure_conversation(
        &mut self,
        ensure: &ConversationEnsure<'_>,
    ) -> Result<ConversationRecord, Error> {
        let found = sqlx::query!(
            "select id, title from rh_conversations
              where session_key = $1 and agent = $2 limit 1",
            ensure.session_key,
            ensure.agent
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation lookup", source))?;
        if let Some(row) = found {
            if let Some(task_id) = ensure.task_id {
                self.backfill_task_id(row.id, task_id).await?;
            }
            return Ok(ConversationRecord {
                id: row.id,
                created: false,
                title: row.title,
            });
        }
        let settings = sqlx::types::Json(ensure.settings);
        let inserted = sqlx::query!(
            "insert into rh_conversations
               (session_key, agent, channel, title, settings, active, created_at, updated_at,
                task_id, owner_user_id)
             values ($1, $2, $3, $4, $5, $6, coalesce($7, now()), coalesce($8, now()),
                     $9::text::uuid, $10)
             returning id",
            ensure.session_key,
            ensure.agent,
            ensure.channel,
            ensure.title,
            settings as _,
            ensure.active,
            ensure.created_at,
            ensure.updated_at,
            ensure.task_id,
            ensure.owner_user_id
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation insert", source))?;
        Ok(ConversationRecord {
            id: inserted.id,
            created: true,
            title: None,
        })
    }

    pub async fn upsert_conversation(
        &mut self,
        upsert: &ConversationUpsert<'_>,
    ) -> Result<UpsertedConversation, Error> {
        let settings = upsert.settings.map(sqlx::types::Json);
        let (imported, created_at, updated_at) = match upsert.dates {
            ConversationDates::CaptureTime => (false, None, None),
            ConversationDates::Imported {
                created_at,
                updated_at,
            } => (true, created_at, updated_at),
        };
        let record = sqlx::query!(
            "insert into rh_conversations (session_key, agent, channel, title, settings,
                                           task_id, owner_user_id, node, created_at, updated_at)
             values ($1, $2, $3, $4, coalesce($5, '{}'::jsonb), $6::text::uuid, $7, $11,
                     coalesce($12, now()), coalesce($13, now()))
             on conflict (session_key, agent) do update
               set updated_at = case when $14
                                     then greatest(rh_conversations.updated_at,
                                                   coalesce($13, rh_conversations.updated_at))
                                     else now() end,
               title = case when $8 then excluded.title
                            else rh_conversations.title end,
               settings = case when $9 then excluded.settings
                               else rh_conversations.settings end,
               task_id = case when $10 then excluded.task_id
                              else rh_conversations.task_id end,
               node = coalesce(rh_conversations.node, excluded.node)
             returning id, owner_user_id",
            upsert.session_key,
            upsert.agent,
            upsert.channel,
            upsert.title,
            settings as _,
            upsert.task_id,
            upsert.owner_user_id,
            upsert.title.is_some(),
            upsert.settings.is_some(),
            upsert.task_id.is_some(),
            upsert.node,
            created_at,
            updated_at,
            imported,
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation upsert", source))?;
        Ok(UpsertedConversation {
            id: record.id,
            owner_user_id: record.owner_user_id,
        })
    }
}

impl crate::PostgresMemory {
    pub(crate) async fn conversation_origins(
        &self,
        session_keys: &[String],
    ) -> Result<std::collections::BTreeMap<String, JsonObject>, Error> {
        if session_keys.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let rows = sqlx::query!(
            "select session_key, settings -> 'origin' as \"origin!: sqlx::types::Json<JsonObject>\"
               from rh_conversations
              where session_key = any($1)
                and jsonb_typeof(settings -> 'origin') = 'object'",
            session_keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("origin lookup", source))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.session_key, row.origin.0))
            .collect())
    }
}

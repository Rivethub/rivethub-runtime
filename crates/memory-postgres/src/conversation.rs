use crate::capture::CaptureWriter;
use crate::capture::ConversationDates;
use crate::capture::ConversationUpsert;
use crate::capture::UpsertedConversation;
use crate::error::Error;
use wire::JsonObject;

impl CaptureWriter<'_> {
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
            crate::time::BindTime::bind_time(created_at) as _,
            crate::time::BindTime::bind_time(updated_at) as _,
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
    ) -> Result<std::collections::BTreeMap<String, kernel::memory::Provenance>, Error> {
        let imported: Vec<&str> = session_keys
            .iter()
            .map(String::as_str)
            .filter(|key| kernel::memory::is_imported(key))
            .collect();
        if imported.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let rows: Vec<(String, Option<sqlx::types::Json<JsonObject>>)> = sqlx::query_as(
            "select session_key, settings from rh_conversations where session_key = any($1)",
        )
        .bind(&imported)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("origin lookup", source))?;
        Ok(rows
            .into_iter()
            .map(|(session_key, settings)| {
                let settings = settings.map(|json| json.0).unwrap_or_default();
                (
                    session_key,
                    kernel::memory::Provenance::from_settings(&settings),
                )
            })
            .collect())
    }
}

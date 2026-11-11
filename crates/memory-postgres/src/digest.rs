use chrono::DateTime;
use chrono::Utc;
use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use uuid::Uuid;

use crate::error::Error;

struct DigestRow {
    key: String,
    conversation_id: Uuid,
    title: Option<String>,
    agent: String,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
    messages: i64,
    imported_from: Option<String>,
}

impl crate::PostgresMemory {
    pub async fn session_digests(
        &self,
        owner: &Owner,
        keys: &[String],
    ) -> Result<Vec<SessionDigest>, Error> {
        let rows = sqlx::query_as!(
            DigestRow,
            "select c.session_key as key, c.id as conversation_id, c.title, c.agent, \
             min(m.created_at) as \"first!\", max(m.created_at) as \"last!\", \
             count(m.id) as \"messages!\", \
             case when c.session_key like 'import:%' \
             then c.settings -> 'origin' ->> 'author' end as imported_from \
             from rh_conversations c join rh_messages m on m.conversation_id = c.id \
             where c.owner_user_id = $1 and c.session_key = any($2) \
             and not exists (select 1 from rh_session_trash tr where tr.session_key = c.session_key) \
             group by c.id \
             order by c.session_key, c.id",
            owner.as_str(),
            keys,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session digest", source))?;
        rows.into_iter()
            .map(|row| {
                Ok(SessionDigest {
                    key: row.key,
                    conversation_id: row.conversation_id,
                    title: row.title.filter(|title| !title.is_empty()),
                    agent: row.agent,
                    first: row.first,
                    last: row.last,
                    messages: u64::try_from(row.messages)
                        .map_err(|source| Error::stored("message count", source))?,
                    imported_from: row.imported_from,
                })
            })
            .collect()
    }
}

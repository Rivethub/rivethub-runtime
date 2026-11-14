use std::collections::BTreeMap;

use crate::{Error, PostgresMemory};

impl PostgresMemory {
    pub(crate) async fn read_recall_identity(
        &self,
    ) -> Result<kernel::memory::RecallIdentity, Error> {
        let principal = sqlx::query_scalar!("SELECT rh_scope_pin()")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("recall principal", source))?;
        let people = sqlx::query_scalar!("SELECT DISTINCT user_id FROM rh_space_members WHERE space_id = ANY (rh_member_spaces()) ORDER BY user_id")
            .fetch_all(&self.pool).await.map_err(|source| Error::operation("recall teammates", source))?;
        Ok(kernel::memory::RecallIdentity { principal, people })
    }

    pub(crate) async fn own_sessions(
        &self,
        keys: &[String],
    ) -> Result<std::collections::BTreeSet<String>, Error> {
        let keys = sqlx::query_scalar!(
            "SELECT DISTINCT session_key FROM rh_conversations WHERE session_key = ANY($1) AND owner_user_id = rh_scope_principal()",
            keys,
        ).fetch_all(&self.pool).await.map_err(|source| Error::operation("own recall sessions", source))?;
        Ok(keys.into_iter().collect())
    }

    pub(crate) async fn team_owners(
        &self,
        keys: &[String],
    ) -> Result<BTreeMap<String, String>, Error> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT session_key, owner_user_id FROM rh_conversations
             WHERE session_key = ANY($1) AND owner_user_id IS NOT NULL
               AND owner_user_id <> rh_scope_principal()",
        )
        .bind(keys)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("recall owners", source))?;
        Ok(rows.into_iter().collect())
    }

    pub(crate) async fn read_team_sessions(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
        before: Option<chrono::DateTime<chrono::Utc>>,
        limit: u32,
    ) -> Result<Vec<kernel::memory::TeamSession>, kernel::memory::MemoryStoreError> {
        let rows = sqlx::query!(
            r#"WITH sessions AS (
                SELECT c.owner_user_id AS owner, c.session_key,
                       max(c.updated_at) AS recent, array_agg(c.id) AS conversations,
                       (array_agg(c.title ORDER BY c.updated_at DESC, c.id)
                        FILTER (WHERE c.title ~ '[^[:space:]]'))[1] AS title
                FROM rh_conversations c
                WHERE c.owner_user_id <> rh_scope_principal() AND c.task_id IS NULL
                  AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key)
                GROUP BY c.owner_user_id, c.session_key
             ), ranked AS (
                SELECT *, row_number() OVER
                    (PARTITION BY owner ORDER BY recent DESC, session_key DESC) AS position
                FROM sessions WHERE ($1::timestamptz IS NULL OR recent >= $1)
                  AND ($2::timestamptz IS NULL OR recent < $2)
             ) SELECT r.owner AS "owner!", r.session_key AS "session_key!", r.recent AS "recent!", r.title,
                (SELECT s.content FROM rh_summaries s WHERE s.conversation_id = ANY(r.conversations)
                 ORDER BY s.depth DESC, s.created_at DESC, s.id LIMIT 1) AS summary,
                (SELECT m.content FROM rh_messages m WHERE m.conversation_id = ANY(r.conversations)
                 AND m.role = 'user' ORDER BY m.created_at, m.id LIMIT 1) AS first_user,
                t.name AS topic_name, parent.name AS parent_name
             FROM ranked r LEFT JOIN rh_session_placements p ON p.session_id = r.session_key
             LEFT JOIN rh_topics t ON t.id = p.topic_id LEFT JOIN rh_topics parent ON parent.id = t.parent_id
             WHERE r.position <= $3 ORDER BY r.owner, r.recent DESC, r.session_key DESC"#,
            since, before, i64::from(limit),
        )
        .fetch_all(&self.pool).await
        .map_err(|source| Error::operation("team sessions", source))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let excerpt = row
                    .summary
                    .as_deref()
                    .or(row.first_user.as_deref())
                    .map(|text| {
                        text.lines()
                            .take(3)
                            .collect::<Vec<_>>()
                            .join("\n")
                            .chars()
                            .take(600)
                            .collect()
                    });
                kernel::memory::TeamSession {
                    title: kernel::memory::session_title(
                        row.title.as_deref(),
                        row.summary.as_deref(),
                        row.first_user.as_deref(),
                        &row.session_key,
                    ),
                    owner: row.owner,
                    session_id: row.session_key,
                    when: row.recent,
                    topic: row.parent_name.into_iter().chain(row.topic_name).collect(),
                    excerpt,
                }
            })
            .collect())
    }
}

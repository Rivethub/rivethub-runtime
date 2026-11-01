use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::owner::Owner;
use kernel::tenancy::SessionVisibility;
use kernel::tenancy::Space;
use kernel::tenancy::SpaceId;
use kernel::tenancy::TenancyStore;
use kernel::tenancy::TenancyStoreError;
use kernel::tenancy::TopicVisibility;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use kernel::tenancy::VisibilityDefault;
use kernel::topics::TopicId;
use sqlx::PgPool;
use wire::session_id::SessionId;

use crate::Error;

#[derive(Debug, Clone)]
pub struct PostgresTenancy {
    pool: PgPool,
}

impl PostgresTenancy {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn failed(operation: &'static str) -> impl FnOnce(sqlx::Error) -> TenancyStoreError {
    move |source| TenancyStoreError::Unavailable(Box::new(Error::operation(operation, source)))
}

fn stored_owner(id: String) -> Result<Owner, TenancyStoreError> {
    Owner::new(id).map_err(|error| TenancyStoreError::Unavailable(Box::new(error)))
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    session: String,
    owner: Option<String>,
    mark: Option<String>,
    effective: String,
}

#[derive(sqlx::FromRow)]
struct TopicRow {
    owner_user_id: Option<String>,
    visibility: String,
    visibility_mark: Option<String>,
}

fn session_row(row: SessionRow) -> Result<(SessionId, SessionVisibility), TenancyStoreError> {
    let session = SessionId::try_from(row.session)
        .map_err(|error| TenancyStoreError::Unavailable(Box::new(error)))?;
    Ok((
        session,
        SessionVisibility {
            owner: row.owner.map(stored_owner).transpose()?,
            mark: row.mark.as_deref().map(Visibility::parse).transpose()?,
            effective: Visibility::parse(&row.effective)?,
        },
    ))
}

const SESSIONS: &str = "WITH keys AS (SELECT DISTINCT key FROM unnest($1::text[]) AS k(key)),
     convs AS (SELECT DISTINCT ON (session_key) session_key, owner_user_id, visibility
                 FROM rh_conversations WHERE session_key = ANY ($1::text[]) ORDER BY session_key, created_at),
     marks AS (SELECT session_key, visibility FROM rh_session_marks
                WHERE owner_user_id = $2 AND session_key = ANY ($1::text[])),
     placed AS (SELECT p.session_id, p.topic_id FROM rh_session_placements p
                 WHERE p.session_id = ANY ($1::text[])
                   AND NOT EXISTS (SELECT 1 FROM convs c WHERE c.session_key = p.session_id)),
     topic_marks AS (SELECT t.topic_id, rh_topic_mark(t.topic_id) AS mark
                       FROM (SELECT DISTINCT topic_id FROM placed) t)
SELECT k.key AS session, c.owner_user_id AS owner, m.visibility AS mark,
       coalesce(c.visibility,
                rh_member_visibility($2, coalesce(m.visibility, tm.mark, rh_owner_default($2, now())))) AS effective
  FROM keys k LEFT JOIN convs c ON c.session_key = k.key LEFT JOIN marks m ON m.session_key = k.key
  LEFT JOIN placed p ON p.session_id = k.key LEFT JOIN topic_marks tm ON tm.topic_id = p.topic_id";

#[async_trait]
impl TenancyStore for PostgresTenancy {
    async fn sync_spaces(&self, spaces: &[Space]) -> Result<(), TenancyStoreError> {
        let pinned: Option<String> = sqlx::query_scalar("SELECT rh_scope_pin()")
            .fetch_one(&self.pool)
            .await
            .map_err(failed("space sync"))?;
        if let Some(person) = pinned {
            tracing::info!(
                "this database login is pinned to {person}; the store's admin manages its spaces"
            );
            return Ok(());
        }
        let mut tx = self.pool.begin().await.map_err(failed("space sync"))?;
        let ids: Vec<&str> = spaces.iter().map(|space| space.id.as_str()).collect();
        sqlx::query("DELETE FROM rh_spaces WHERE NOT (id = ANY ($1::text[]))")
            .bind(&ids)
            .execute(&mut *tx)
            .await
            .map_err(failed("space sync"))?;
        for space in spaces {
            sqlx::query(
                "INSERT INTO rh_spaces (id, kind, name) VALUES ($1, $2, $3)
                 ON CONFLICT (id) DO UPDATE SET kind = EXCLUDED.kind, name = EXCLUDED.name
                  WHERE (rh_spaces.kind, rh_spaces.name) IS DISTINCT FROM (EXCLUDED.kind, EXCLUDED.name)",
            )
            .bind(space.id.as_str())
            .bind(space.kind.as_str())
            .bind(&space.name)
            .execute(&mut *tx)
            .await
            .map_err(failed("space sync"))?;
            let members: Vec<&str> = space.members.iter().map(Owner::as_str).collect();
            sqlx::query(
                "DELETE FROM rh_space_members WHERE space_id = $1 AND NOT (user_id = ANY ($2::text[]))",
            )
            .bind(space.id.as_str())
            .bind(&members)
            .execute(&mut *tx)
            .await
            .map_err(failed("space sync"))?;
            sqlx::query(
                "INSERT INTO rh_space_members (space_id, user_id)
                 SELECT $1, member FROM unnest($2::text[]) AS member
                 ON CONFLICT (space_id, user_id) DO NOTHING",
            )
            .bind(space.id.as_str())
            .bind(&members)
            .execute(&mut *tx)
            .await
            .map_err(failed("space sync"))?;
        }
        tx.commit().await.map_err(failed("space sync"))
    }

    async fn default_visibility(
        &self,
        user: &Owner,
    ) -> Result<Option<VisibilityDefault>, TenancyStoreError> {
        let row = sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT default_visibility, default_since FROM rh_user_settings WHERE user_id = $1",
        )
        .bind(user.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("visibility default"))?;
        row.map(|(visibility, since)| {
            Ok(VisibilityDefault {
                visibility: Visibility::parse(&visibility)?,
                since,
            })
        })
        .transpose()
    }

    async fn set_default_visibility(
        &self,
        user: &Owner,
        visibility: &Visibility,
        now: DateTime<Utc>,
    ) -> Result<VisibilityDefault, TenancyStoreError> {
        let (stored, since) = sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "INSERT INTO rh_user_settings (user_id, default_visibility, default_since, updated_at)
             VALUES ($1, $2, $3, $3)
             ON CONFLICT (user_id) DO UPDATE SET
                default_since = CASE WHEN rh_user_settings.default_visibility = EXCLUDED.default_visibility
                                     THEN rh_user_settings.default_since ELSE EXCLUDED.default_since END,
                default_visibility = EXCLUDED.default_visibility,
                updated_at = EXCLUDED.updated_at
             RETURNING default_visibility, default_since",
        )
        .bind(user.as_str())
        .bind(visibility.as_str())
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(failed("visibility default"))?;
        Ok(VisibilityDefault {
            visibility: Visibility::parse(&stored)?,
            since,
        })
    }

    async fn session_visibility(
        &self,
        viewer: &Viewer,
        sessions: &[SessionId],
    ) -> Result<BTreeMap<SessionId, SessionVisibility>, TenancyStoreError> {
        let keys: Vec<&str> = sessions.iter().map(SessionId::as_str).collect();
        sqlx::query_as::<_, SessionRow>(SESSIONS)
            .bind(&keys)
            .bind(viewer.principal().as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(failed("session visibility"))?
            .into_iter()
            .map(session_row)
            .collect()
    }

    async fn mark_session(
        &self,
        viewer: &Viewer,
        session: &SessionId,
        mark: Option<&Visibility>,
        now: DateTime<Utc>,
    ) -> Result<SessionVisibility, TenancyStoreError> {
        if let Some(mark) = mark {
            viewer.may_share(mark)?;
        }
        let mut tx = self.pool.begin().await.map_err(failed("session mark"))?;
        let owner = sqlx::query_as::<_, (String,)>(
            "SELECT owner_user_id FROM rh_conversations WHERE session_key = $1 ORDER BY created_at LIMIT 1",
        )
        .bind(session.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(failed("session mark"))?;
        if owner.is_some_and(|(owner,)| owner != viewer.principal().as_str()) {
            return Err(TenancyStoreError::NotOwner(session.as_str().to_owned()));
        }
        match mark {
            Some(mark) => sqlx::query(
                "INSERT INTO rh_session_marks (session_key, owner_user_id, visibility, marked_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (session_key, owner_user_id)
                 DO UPDATE SET visibility = EXCLUDED.visibility, marked_at = EXCLUDED.marked_at",
            )
            .bind(session.as_str())
            .bind(viewer.principal().as_str())
            .bind(mark.as_str())
            .bind(now),
            None => sqlx::query(
                "DELETE FROM rh_session_marks WHERE session_key = $1 AND owner_user_id = $2",
            )
            .bind(session.as_str())
            .bind(viewer.principal().as_str()),
        }
        .execute(&mut *tx)
        .await
        .map_err(failed("session mark"))?;
        let row = sqlx::query_as::<_, SessionRow>(SESSIONS)
            .bind(vec![session.as_str()])
            .bind(viewer.principal().as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(failed("session mark"))?;
        tx.commit().await.map_err(failed("session mark"))?;
        session_row(row).map(|(_, visibility)| visibility)
    }

    async fn mark_topic(
        &self,
        viewer: &Viewer,
        topic: TopicId,
        mark: Option<&Visibility>,
    ) -> Result<TopicVisibility, TenancyStoreError> {
        if let Some(mark) = mark {
            viewer.may_share(mark)?;
        }
        let mut tx = self.pool.begin().await.map_err(failed("topic mark"))?;
        let found = sqlx::query_as::<_, TopicRow>(
            "SELECT owner_user_id, visibility, visibility_mark FROM rh_topics WHERE id = $1",
        )
        .bind(topic.uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(failed("topic mark"))?;
        let Some(found) = found else {
            return Err(TenancyStoreError::NotFound(topic.to_string()));
        };
        if found.owner_user_id.as_deref() != Some(viewer.principal().as_str()) {
            return Err(TenancyStoreError::NotOwner(topic.to_string()));
        }
        sqlx::query("UPDATE rh_topics SET visibility_mark = $2 WHERE id = $1")
            .bind(topic.uuid())
            .bind(mark.map(Visibility::as_str))
            .execute(&mut *tx)
            .await
            .map_err(failed("topic mark"))?;
        let row = sqlx::query_as::<_, TopicRow>(
            "SELECT owner_user_id, visibility, visibility_mark FROM rh_topics WHERE id = $1",
        )
        .bind(topic.uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(failed("topic mark"))?;
        tx.commit().await.map_err(failed("topic mark"))?;
        Ok(TopicVisibility {
            topic,
            mark: row
                .visibility_mark
                .as_deref()
                .map(Visibility::parse)
                .transpose()?,
            effective: Visibility::parse(&row.visibility)?,
        })
    }

    async fn viewer_of(&self, owner: &Owner) -> Result<Viewer, TenancyStoreError> {
        let spaces = sqlx::query_as::<_, (String,)>(
            "SELECT space_id FROM rh_space_members WHERE user_id = $1 ORDER BY space_id",
        )
        .bind(owner.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(failed("viewer"))?
        .into_iter()
        .map(|(space,)| SpaceId::new(space))
        .collect::<Result<Vec<_>, _>>()?;
        Ok(Viewer::new(owner.clone(), spaces))
    }

    async fn conversation_owner(
        &self,
        conversation: uuid::Uuid,
    ) -> Result<Option<Owner>, TenancyStoreError> {
        sqlx::query_as::<_, (String,)>("SELECT owner_user_id FROM rh_conversations WHERE id = $1")
            .bind(conversation)
            .fetch_optional(&self.pool)
            .await
            .map_err(failed("conversation owner"))?
            .map(|(owner,)| stored_owner(owner))
            .transpose()
    }
}

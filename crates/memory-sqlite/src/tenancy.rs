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
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use wire::session_id::SessionKey;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const MAX_DEPTH: usize = 64;

fn query(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> SqliteError {
    move |source| SqliteError::Query { operation, source }
}

fn unavailable(error: SqliteError) -> TenancyStoreError {
    TenancyStoreError::Unavailable(Box::new(error))
}

fn default_of(
    conn: &Connection,
    user: &str,
) -> Result<Option<(String, DateTime<Utc>)>, SqliteError> {
    let row = conn
        .query_row(
            "SELECT default_visibility, default_since FROM rh_user_settings WHERE user_id = ?1",
            params![user],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(query("visibility default"))?;
    row.map(|(visibility, since)| Ok((visibility, time::stored(&since, "default_since")?)))
        .transpose()
}

fn topic_mark(conn: &Connection, topic: Option<String>) -> Result<Option<String>, SqliteError> {
    let mut next = topic;
    for _ in 0..MAX_DEPTH {
        let Some(id) = next else {
            return Ok(None);
        };
        let row = conn
            .query_row(
                "SELECT visibility_mark, parent_id FROM rh_topics WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(query("topic mark"))?;
        match row {
            None => return Ok(None),
            Some((Some(mark), _)) => return Ok(Some(mark)),
            Some((None, parent)) => next = parent,
        }
    }
    Ok(None)
}

fn owner_default(
    conn: &Connection,
    owner: &str,
    created: DateTime<Utc>,
) -> Result<String, SqliteError> {
    Ok(match default_of(conn, owner)? {
        Some((visibility, since)) if created >= since => visibility,
        Some(_) | None => Visibility::Private.as_str().to_owned(),
    })
}

fn member_visibility(
    conn: &Connection,
    owner: &str,
    wanted: String,
) -> Result<String, SqliteError> {
    if wanted == Visibility::Private.as_str() {
        return Ok(wanted);
    }
    let member: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM rh_space_members WHERE space_id = ?1 AND user_id = ?2)",
            params![wanted, owner],
            |row| row.get(0),
        )
        .map_err(query("space membership"))?;
    Ok(if member {
        wanted
    } else {
        Visibility::Private.as_str().to_owned()
    })
}

fn session_state(
    conn: &Connection,
    session: &str,
    viewer: &str,
    now: DateTime<Utc>,
) -> Result<(Option<String>, Option<String>, String), SqliteError> {
    let conversation = conn
        .query_row(
            "SELECT owner_user_id, created_at FROM ros_conversations WHERE session_key = ?1
              ORDER BY created_at LIMIT 1",
            params![session],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(query("session visibility"))?;
    let (owner, created) = match conversation {
        Some((owner, created)) => (owner, time::stored(&created, "created_at")?),
        None => (None, now),
    };
    let subject = owner.clone().unwrap_or_else(|| viewer.to_owned());
    let mark: Option<String> = conn
        .query_row(
            "SELECT visibility FROM rh_session_marks WHERE session_key = ?1 AND owner_user_id = ?2",
            params![session, subject],
            |row| row.get(0),
        )
        .optional()
        .map_err(query("session visibility"))?;
    let placed: Option<String> = conn
        .query_row(
            "SELECT topic_id FROM rh_session_placements WHERE session_id = ?1",
            params![session],
            |row| row.get(0),
        )
        .optional()
        .map_err(query("session visibility"))?;
    let wanted = match (&mark, topic_mark(conn, placed)?) {
        (Some(mark), _) => mark.clone(),
        (None, Some(topic)) => topic,
        (None, None) => owner_default(conn, &subject, created)?,
    };
    let effective = member_visibility(conn, &subject, wanted)?;
    Ok((owner, mark, effective))
}

fn refresh_topics(conn: &Connection) -> Result<(), SqliteError> {
    let topics = conn
        .prepare(
            "SELECT id, owner_user_id, created_at FROM rh_topics WHERE owner_user_id IS NOT NULL",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(query("topic visibility"))?;
    for (id, owner, created) in topics {
        let wanted = match topic_mark(conn, Some(id.clone()))? {
            Some(mark) => mark,
            None => owner_default(conn, &owner, time::stored(&created, "created_at")?)?,
        };
        let effective = member_visibility(conn, &owner, wanted)?;
        conn.execute(
            "UPDATE rh_topics SET visibility = ?2 WHERE id = ?1 AND visibility <> ?2",
            params![id, effective],
        )
        .map_err(query("topic visibility"))?;
    }
    Ok(())
}

fn session_visibility(
    owner: Option<String>,
    mark: Option<&str>,
    effective: &str,
) -> Result<SessionVisibility, TenancyStoreError> {
    Ok(SessionVisibility {
        owner: owner
            .map(Owner::new)
            .transpose()
            .map_err(|error| TenancyStoreError::Unavailable(Box::new(error)))?,
        mark: mark.map(Visibility::parse).transpose()?,
        effective: Visibility::parse(effective)?,
    })
}

#[async_trait]
impl TenancyStore for SqliteStore {
    async fn sync_spaces(&self, _node: &str, spaces: &[Space]) -> Result<(), TenancyStoreError> {
        let spaces = spaces.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.transaction().map_err(query("space sync"))?;
            tx.execute("DELETE FROM rh_space_members", [])
                .map_err(query("space sync"))?;
            tx.execute("DELETE FROM rh_spaces", [])
                .map_err(query("space sync"))?;
            for space in &spaces {
                tx.execute(
                    "INSERT INTO rh_spaces (id, kind, name) VALUES (?1, ?2, ?3)",
                    params![space.id.as_str(), space.kind.as_str(), space.name],
                )
                .map_err(query("space sync"))?;
                for member in &space.members {
                    tx.execute(
                        "INSERT OR IGNORE INTO rh_space_members (space_id, user_id) VALUES (?1, ?2)",
                        params![space.id.as_str(), member.as_str()],
                    )
                    .map_err(query("space sync"))?;
                }
            }
            refresh_topics(&tx)?;
            tx.commit().map_err(query("space sync"))
        })
        .await
        .map_err(unavailable)
    }

    async fn default_visibility(
        &self,
        user: &Owner,
    ) -> Result<Option<VisibilityDefault>, TenancyStoreError> {
        let user = user.as_str().to_owned();
        let found = self
            .with_conn(move |conn| default_of(conn, &user))
            .await
            .map_err(unavailable)?;
        found
            .map(|(visibility, since)| {
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
        let user = user.as_str().to_owned();
        let wanted = visibility.as_str().to_owned();
        let (stored, since) = self
            .with_conn(move |conn| {
                let tx = conn.transaction().map_err(query("visibility default"))?;
                let since = match default_of(&tx, &user)? {
                    Some((current, since)) if current == wanted => since,
                    Some(_) | None => now,
                };
                tx.execute(
                    "INSERT INTO rh_user_settings (user_id, default_visibility, default_since, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (user_id) DO UPDATE SET default_visibility = excluded.default_visibility,
                        default_since = excluded.default_since, updated_at = excluded.updated_at",
                    params![user, wanted, time::stamp(since), time::stamp(now)],
                )
                .map_err(query("visibility default"))?;
                refresh_topics(&tx)?;
                tx.commit().map_err(query("visibility default"))?;
                Ok((wanted, since))
            })
            .await
            .map_err(unavailable)?;
        Ok(VisibilityDefault {
            visibility: Visibility::parse(&stored)?,
            since,
        })
    }

    async fn session_visibility(
        &self,
        viewer: &Viewer,
        sessions: &[SessionKey],
    ) -> Result<BTreeMap<SessionKey, SessionVisibility>, TenancyStoreError> {
        let viewer = viewer.principal().as_str().to_owned();
        let sessions = sessions.to_vec();
        let now = self.now();
        let rows = self
            .with_conn(move |conn| {
                sessions
                    .into_iter()
                    .map(|session| {
                        let state = session_state(conn, session.as_str(), &viewer, now)?;
                        Ok((session, state))
                    })
                    .collect::<Result<Vec<_>, SqliteError>>()
            })
            .await
            .map_err(unavailable)?;
        rows.into_iter()
            .map(|(session, (owner, mark, effective))| {
                Ok((
                    session,
                    session_visibility(owner, mark.as_deref(), &effective)?,
                ))
            })
            .collect()
    }

    async fn mark_session(
        &self,
        viewer: &Viewer,
        session: &SessionKey,
        mark: Option<&Visibility>,
        now: DateTime<Utc>,
    ) -> Result<SessionVisibility, TenancyStoreError> {
        if let Some(mark) = mark {
            viewer.may_share(mark)?;
        }
        let principal = viewer.principal().as_str().to_owned();
        let key = session.as_str().to_owned();
        let mark = mark.map(|mark| mark.as_str().to_owned());
        let (owner, mark, effective) = self
            .with_conn(move |conn| {
                let tx = conn.transaction().map_err(query("session mark"))?;
                let (owner, _, _) = session_state(&tx, &key, &principal, now)?;
                if owner.as_ref().is_some_and(|owner| owner != &principal) {
                    return Ok(None);
                }
                match &mark {
                    Some(mark) => tx.execute(
                        "INSERT INTO rh_session_marks (session_key, owner_user_id, visibility, marked_at)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT (session_key, owner_user_id)
                         DO UPDATE SET visibility = excluded.visibility, marked_at = excluded.marked_at",
                        params![key, principal, mark, time::stamp(now)],
                    ),
                    None => tx.execute(
                        "DELETE FROM rh_session_marks WHERE session_key = ?1 AND owner_user_id = ?2",
                        params![key, principal],
                    ),
                }
                .map_err(query("session mark"))?;
                let state = session_state(&tx, &key, &principal, now)?;
                tx.commit().map_err(query("session mark"))?;
                Ok(Some(state))
            })
            .await
            .map_err(unavailable)?
            .ok_or_else(|| TenancyStoreError::NotOwner(session.as_str().to_owned()))?;
        session_visibility(owner, mark.as_deref(), &effective)
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
        let principal = viewer.principal().as_str().to_owned();
        let id = topic.to_string();
        let mark = mark.map(|mark| mark.as_str().to_owned());
        let found = self
            .with_conn(move |conn| {
                let tx = conn.transaction().map_err(query("topic mark"))?;
                let owner: Option<Option<String>> = tx
                    .query_row(
                        "SELECT owner_user_id FROM rh_topics WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(query("topic mark"))?;
                match owner {
                    None => return Ok(Err(false)),
                    Some(owner) if owner.as_deref() != Some(principal.as_str()) => {
                        return Ok(Err(true));
                    }
                    Some(_) => {}
                }
                tx.execute(
                    "UPDATE rh_topics SET visibility_mark = ?2 WHERE id = ?1",
                    params![id, mark],
                )
                .map_err(query("topic mark"))?;
                refresh_topics(&tx)?;
                let row = tx
                    .query_row(
                        "SELECT visibility, visibility_mark FROM rh_topics WHERE id = ?1",
                        params![id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .map_err(query("topic mark"))?;
                tx.commit().map_err(query("topic mark"))?;
                Ok(Ok(row))
            })
            .await
            .map_err(unavailable)?;
        match found {
            Ok((effective, mark)) => Ok(TopicVisibility {
                topic,
                mark: mark.as_deref().map(Visibility::parse).transpose()?,
                effective: Visibility::parse(&effective)?,
            }),
            Err(true) => Err(TenancyStoreError::NotOwner(topic.to_string())),
            Err(false) => Err(TenancyStoreError::NotFound(topic.to_string())),
        }
    }

    async fn viewer_of(&self, owner: &Owner) -> Result<Viewer, TenancyStoreError> {
        let user = owner.as_str().to_owned();
        let spaces = self
            .with_conn(move |conn| {
                conn.prepare(
                    "SELECT space_id FROM rh_space_members WHERE user_id = ?1 ORDER BY space_id",
                )
                .and_then(|mut statement| {
                    statement
                        .query_map(params![user], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(query("viewer"))
            })
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(SpaceId::new)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Viewer::new(owner.clone(), spaces))
    }

    async fn conversation_owner(
        &self,
        conversation: uuid::Uuid,
    ) -> Result<Option<Owner>, TenancyStoreError> {
        let id = conversation.to_string();
        let owner: Option<Option<String>> = self
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT owner_user_id FROM ros_conversations WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(query("conversation owner"))
            })
            .await
            .map_err(unavailable)?;
        owner
            .flatten()
            .map(Owner::new)
            .transpose()
            .map_err(|error| TenancyStoreError::Unavailable(Box::new(error)))
    }
}

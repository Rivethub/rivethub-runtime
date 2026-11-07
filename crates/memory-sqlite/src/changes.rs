use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use kernel::changes::ChangeError;
use kernel::changes::ChangeFeed;
use kernel::changes::ChangeRecord;
use kernel::changes::ChangeWindow;
use kernel::changes::DailyDigest;
use kernel::changes::DigestRequest;
use kernel::changes::DigestStore;
use kernel::changes::TopicRef;
use kernel::owner::Owner;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::named_params;
use rusqlite::params;
use wire::session_id::SessionId;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const IN_WINDOW: &str =
    "(:from IS NULL OR julianday(AT) >= julianday(:from)) AND julianday(AT) < julianday(:to)";

const CLAIMS_SQL: &str = "SELECT (SELECT g.id FROM rh_entity_facts g
                   WHERE g.topic_id = f.topic_id AND g.key = f.key
                   ORDER BY g.created_at, g.id LIMIT 1),
            f.key, c.value, c.actor, c.pinned, c.session_id, c.summary_id, c.evidence,
            c.at, c.recorded_at, c.owner_user_id, t.id, t.name
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
       JOIN rh_topics t ON t.id = f.topic_id
      WHERE julianday(c.at) < julianday(:to)
        AND f.topic_id IN (SELECT w.topic_id FROM rh_entity_facts w
                             JOIN rh_entity_fact_claims n ON n.fact_id = w.id
                            WHERE WINDOW)
      ORDER BY julianday(c.at), julianday(c.recorded_at), c.id";

const DECISIONS_SQL: &str = "SELECT t.id, t.name, t.kind, d.text, d.session_id, d.created_at
       FROM rh_topic_decisions d JOIN rh_topics t ON t.id = d.topic_id
      WHERE WINDOW";

const CREATED_SQL: &str = "SELECT t.id, t.name, t.kind, t.created_at FROM rh_topics t
      WHERE t.kind IN ('work', 'entity') AND WINDOW";

const PLACED_SQL: &str = "SELECT t.id, t.name, t.kind, p.session_id, p.placed_by, p.placed_at
       FROM rh_session_placements p JOIN rh_topics t ON t.id = p.topic_id
      WHERE WINDOW";

const TAGGED_SQL: &str = "SELECT k.session_key, k.key, k.value, k.at FROM (
         SELECT coalesce(c.session_key, sc.session_key) AS session_key, g.key, g.value,
                coalesce(g.decided_at, g.updated_at) AS at
           FROM ros_tags g
           LEFT JOIN ros_conversations c ON g.entity_type = 'conversation' AND c.id = g.entity_id
           LEFT JOIN ros_summaries s ON g.entity_type = 'summary' AND s.id = g.entity_id
           LEFT JOIN ros_conversations sc ON sc.id = s.conversation_id
          WHERE g.state = 'accepted'
     ) k
      WHERE k.session_key IS NOT NULL
        AND substr(k.session_key, 1, instr(k.session_key, ':') - 1) IN (SELECT value FROM json_each(:harnesses))
        AND WINDOW";

const MERGED_SQL: &str = "SELECT t.id, t.name, t.kind, r.merged_from, r.created_at
       FROM rh_topic_redirects r JOIN rh_topics t ON t.id = r.topic_id
      WHERE r.merged_from IS NOT NULL AND WINDOW";

fn windowed(sql: &str, column: &str) -> String {
    sql.replace("WINDOW", &IN_WINDOW.replace("AT", column))
}

fn unavailable(source: impl std::error::Error + Send + Sync + 'static) -> ChangeError {
    ChangeError::Unavailable(Box::new(source))
}

fn invalid(field: &str, error: impl std::fmt::Display) -> SqliteError {
    SqliteError::Invalid {
        message: format!("stored {field} is invalid: {error}"),
    }
}

fn topic(id: &str, title: String, kind: &str) -> Result<TopicRef, SqliteError> {
    Ok(TopicRef {
        id: TopicId::from_str(id).map_err(|error| invalid("topic id", error))?,
        title,
        kind: TopicKind::from_str(kind).map_err(|error| invalid("topic kind", error))?,
    })
}

fn session(key: &str) -> Result<SessionId, SqliteError> {
    SessionId::from_str(key).map_err(|error| invalid("session id", error))
}

fn stored(text: &str) -> Result<DateTime<Utc>, SqliteError> {
    time::stored(text, "change time")
}

struct Bounds {
    from: Option<String>,
    to: String,
    harnesses: String,
}

fn rows<T>(
    conn: &Connection,
    sql: &str,
    bounds: &Bounds,
    read: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, SqliteError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(SqliteError::query("change feed"))?;
    let mut bound: Vec<(&str, &dyn rusqlite::ToSql)> =
        vec![(":from", &bounds.from), (":to", &bounds.to)];
    if statement
        .parameter_index(":harnesses")
        .map_err(SqliteError::query("change feed"))?
        .is_some()
    {
        bound.push((":harnesses", &bounds.harnesses));
    }
    statement
        .query_map(bound.as_slice(), read)
        .map_err(SqliteError::query("change feed"))?
        .collect::<rusqlite::Result<Vec<T>>>()
        .map_err(SqliteError::query("change feed"))
}

fn claims(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(conn, &windowed(CLAIMS_SQL, "n.at"), bounds, |row| {
        Ok((
            crate::facts::read_claim(row)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
        ))
    })?;
    found
        .into_iter()
        .map(|(row, entity, title)| {
            Ok(ChangeRecord::Claim {
                entity: TopicId::from_str(&entity).map_err(|error| invalid("entity id", error))?,
                title,
                claim: crate::facts::claim(&row).map_err(|error| invalid("fact claim", error))?,
            })
        })
        .collect()
}

fn decisions(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(
        conn,
        &windowed(DECISIONS_SQL, "d.created_at"),
        bounds,
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    )?;
    found
        .into_iter()
        .map(|(id, title, kind, text, session_id, at)| {
            Ok(ChangeRecord::Decision {
                topic: topic(&id, title, &kind)?,
                text,
                session: session_id.as_deref().map(session).transpose()?,
                at: stored(&at)?,
            })
        })
        .collect()
}

fn created(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(
        conn,
        &windowed(CREATED_SQL, "t.created_at"),
        bounds,
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    found
        .into_iter()
        .map(|(id, title, kind, at)| {
            Ok(ChangeRecord::Created {
                topic: topic(&id, title, &kind)?,
                at: stored(&at)?,
            })
        })
        .collect()
}

fn placed(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(conn, &windowed(PLACED_SQL, "p.placed_at"), bounds, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    found
        .into_iter()
        .map(|(id, title, kind, session_id, by, at)| {
            Ok(ChangeRecord::Placed {
                topic: topic(&id, title, &kind)?,
                session: session(&session_id)?,
                by: PlacementSource::from_str(&by).map_err(|error| invalid("placed_by", error))?,
                at: stored(&at)?,
            })
        })
        .collect()
}

fn tagged(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(conn, &windowed(TAGGED_SQL, "k.at"), bounds, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    found
        .into_iter()
        .map(|(key, tag, value, at)| {
            Ok(ChangeRecord::Tagged {
                session: session(&key)?,
                key: tag,
                value,
                at: stored(&at)?,
            })
        })
        .collect()
}

fn merged(conn: &Connection, bounds: &Bounds) -> Result<Vec<ChangeRecord>, SqliteError> {
    let found = rows(conn, &windowed(MERGED_SQL, "r.created_at"), bounds, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    found
        .into_iter()
        .map(|(id, title, kind, from, at)| {
            Ok(ChangeRecord::Merged {
                into: topic(&id, title, &kind)?,
                from,
                at: stored(&at)?,
            })
        })
        .collect()
}

fn records(conn: &Connection, window: &ChangeWindow) -> Result<Vec<ChangeRecord>, SqliteError> {
    let bounds = Bounds {
        from: window.from.map(time::stamp),
        to: time::stamp(window.to),
        harnesses: crate::session_catalog::harnesses(None)?,
    };
    let mut found = claims(conn, &bounds)?;
    found.extend(decisions(conn, &bounds)?);
    found.extend(created(conn, &bounds)?);
    found.extend(placed(conn, &bounds)?);
    found.extend(tagged(conn, &bounds)?);
    found.extend(merged(conn, &bounds)?);
    Ok(found)
}

#[async_trait]
impl ChangeFeed for SqliteStore {
    async fn records(&self, window: &ChangeWindow) -> Result<Vec<ChangeRecord>, ChangeError> {
        let window = window.clone();
        self.with_conn(move |conn| records(conn, &window))
            .await
            .map_err(unavailable)
    }
}

fn digest_row(
    conn: &Connection,
    day: NaiveDate,
    owner: &Owner,
) -> Result<Option<DailyDigest>, SqliteError> {
    let found: Option<(String, u32, String)> = conn
        .query_row(
            "SELECT summary, changes, written_at FROM rh_daily_digests
              WHERE owner_user_id = ?1 AND day = ?2",
            params![owner.as_str(), day.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(SqliteError::query("daily digest"))?;
    found
        .map(|(summary, changes, written_at)| {
            Ok(DailyDigest {
                day,
                owner: owner.clone(),
                summary,
                changes,
                written_at: time::stored(&written_at, "written_at")?,
            })
        })
        .transpose()
}

#[async_trait]
impl DigestStore for SqliteStore {
    async fn digest(
        &self,
        day: NaiveDate,
        owner: &Owner,
    ) -> Result<Option<DailyDigest>, ChangeError> {
        let owner = owner.clone();
        self.with_conn(move |conn| digest_row(conn, day, &owner))
            .await
            .map_err(unavailable)
    }

    async fn keep_digest(&self, digest: &DailyDigest) -> Result<(), ChangeError> {
        let digest = digest.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO rh_daily_digests (owner_user_id, day, summary, changes, written_at)
                 VALUES (:owner, :day, :summary, :changes, :written_at)
                 ON CONFLICT (owner_user_id, day) DO UPDATE SET summary = excluded.summary,
                        changes = excluded.changes, written_at = excluded.written_at",
                named_params! {
                    ":owner": digest.owner.as_str(),
                    ":day": digest.day.to_string(),
                    ":summary": digest.summary,
                    ":changes": digest.changes,
                    ":written_at": time::stamp(digest.written_at),
                },
            )
            .map_err(SqliteError::query("daily digest"))
            .map(|_| ())
        })
        .await
        .map_err(unavailable)
    }

    async fn request_digest(&self, request: &DigestRequest) -> Result<(), ChangeError> {
        let job = request.job()?;
        let payload = serde_json::to_string(&job.payload).map_err(unavailable)?;
        let insert = crate::port::job_insert(&job, payload, Utc::now()).map_err(unavailable)?;
        self.with_conn(move |conn| crate::queue::enqueue_in(conn, &insert))
            .await
            .map_err(unavailable)
    }
}

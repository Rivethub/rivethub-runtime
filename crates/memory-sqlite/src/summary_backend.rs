use kernel::memory::summary_backend::SummaryArtifact;
use kernel::memory::{
    SummaryKind,
    summary_backend::SummaryInput,
    summary_reference::{PreparedSummaryRecord, SummaryManifestEntry},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

use crate::{SqliteError, settlement::INCLUDED, summary_replace::publish::stored_uuid};

pub(crate) fn error(source: rusqlite::Error) -> SqliteError {
    SqliteError::query("live summary workspace")(source)
}

const ELIGIBLE: &str = "s.input_revision = ?2 AND s.settled_at IS NOT NULL
    AND s.summarized_through IS NOT ?2 AND s.failed_revision IS NOT ?2";

pub(crate) fn eligible(conn: &Connection, id: Uuid, revision: i64) -> Result<bool, SqliteError> {
    conn.query_row(&format!(
        "SELECT EXISTS (SELECT 1 FROM rh_summary_state s JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE c.id = ?1 AND {ELIGIBLE} AND {INCLUDED})"
    ), params![id.to_string(), revision], |row| row.get(0)).map_err(error)
}

pub(crate) fn candidates(
    conn: &Connection,
    after: Uuid,
    limit: i64,
) -> Result<Vec<(Uuid, i64)>, SqliteError> {
    let mut statement = conn.prepare(&format!(
        "SELECT c.id, s.input_revision FROM rh_summary_state s JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE c.id > ?1 AND s.settled_at IS NOT NULL AND NOT s.historical_only
           AND s.input_revision IS NOT s.summarized_through AND s.input_revision IS NOT s.failed_revision
           AND NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = 'compact-' || c.id
             AND j.state IN ('running', 'queued'))
           AND {INCLUDED} ORDER BY c.id LIMIT ?2"
    )).map_err(error)?;
    let rows = statement
        .query_map(params![after.to_string(), limit], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?;
    rows.map(|row| {
        let (id, revision) = row.map_err(error)?;
        Ok((stored_uuid(id)?, revision))
    })
    .collect()
}

pub(crate) fn input(conn: &Connection, id: Uuid) -> Result<Option<SummaryInput>, SqliteError> {
    let owner: Option<String> = conn
        .query_row(
            "SELECT coalesce(owner_user_id, '') FROM ros_conversations WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(owner) = owner else { return Ok(None) };
    let rows = conn
        .prepare("SELECT id, kind FROM ros_summaries WHERE conversation_id = ?1")
        .and_then(|mut stmt| {
            stmt.query_map([id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .map_err(error)?;
    let expected_root = rows
        .iter()
        .find(|(_, kind)| matches!(kind.as_str(), "session" | "root"))
        .map(|(id, _)| stored_uuid(id.clone()))
        .transpose()?;
    let expected_ids = rows
        .into_iter()
        .map(|(id, _)| stored_uuid(id))
        .collect::<Result<_, _>>()?;
    let messages = crate::session_messages::summary_messages(conn, id)?;
    let parts = parts(conn, id)?;
    Ok(Some(SummaryInput {
        owner,
        expected_ids,
        expected_root,
        messages,
        parts,
    }))
}

fn parts(conn: &Connection, id: Uuid) -> Result<Vec<PreparedSummaryRecord>, SqliteError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, parent_id, summary_ordinal, content, input_key, index_key, model,
                issued_part_ordinal, input_token_estimate, quality_flags
         FROM ros_summaries WHERE conversation_id = ?1 AND kind = 'part' ORDER BY summary_ordinal",
        )
        .map_err(error)?;
    let mut rows = stmt.query([id.to_string()]).map_err(error)?;
    let mut parts = Vec::new();
    while let Some(row) = rows.next().map_err(error)? {
        let id = stored_uuid(row.get(0).map_err(error)?)?;
        let parent = row
            .get::<_, Option<String>>(1)
            .map_err(error)?
            .map(stored_uuid)
            .transpose()?;
        let Some(input_key) = key(row.get(4).map_err(error)?) else {
            continue;
        };
        let mut stmt = conn.prepare(
            "SELECT message_id, ordinal, render_key FROM ros_summary_sources WHERE summary_id = ?1 ORDER BY ordinal"
        ).map_err(error)?;
        let links = stmt
            .query_map([id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(error)?;
        let sources = links
            .map(|link| {
                let (message, ordinal, render) = link.map_err(error)?;
                Ok(SummaryManifestEntry {
                    message: stored_uuid(message)?,
                    ordinal,
                    render_key: key(render),
                })
            })
            .collect::<Result<_, SqliteError>>()?;
        parts.push(PreparedSummaryRecord {
            id,
            parent,
            kind: SummaryKind::Part,
            sources,
            input_key,
            ordinal: row.get(2).map_err(error)?,
            content: row.get(3).map_err(error)?,
            index_key: key(row.get(5).map_err(error)?),
            model: row.get(6).map_err(error)?,
            issued_part_ordinal: row.get(7).map_err(error)?,
            input_tokens: row
                .get::<_, Option<i64>>(8)
                .map_err(error)?
                .map(u64::try_from)
                .transpose()
                .map_err(crate::SqliteError::Count)?,
            quality_flags: row.get(9).map_err(error)?,
            section: None,
        });
    }
    Ok(parts)
}

fn key(bytes: Option<Vec<u8>>) -> Option<[u8; 32]> {
    let bytes = bytes?;
    match bytes.try_into() {
        Ok(key) => Some(key),
        Err(bytes) => {
            tracing::debug!(length = bytes.len(), "summary key has the wrong length");
            None
        }
    }
}

pub(crate) fn freeze(
    conn: &Connection,
    id: Uuid,
    revision: i64,
    root: Option<Uuid>,
    manifest: &SummaryArtifact,
) -> Result<Uuid, SqliteError> {
    let id: String = conn.query_row(
                "INSERT INTO rh_summary_runs (id, conversation_id, revision, expected_root, manifest, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (conversation_id) DO UPDATE SET revision = excluded.revision,
                     expected_root = excluded.expected_root, manifest = excluded.manifest
                 WHERE rh_summary_runs.revision <= excluded.revision RETURNING id",
                params![Uuid::new_v4().to_string(), id.to_string(), revision, root.map(|root| root.to_string()),
                    manifest.as_json(), crate::time::stamp(jiff::Timestamp::now())], |row| row.get(0)
            ).map_err(error)?;
    stored_uuid(id)
}

use kernel::filing::Filable;
use kernel::memory::summary_opening::ArmOpeningOutcome;
use kernel::memory::summary_opening::OpeningTitle;
use kernel::memory::{ConversationHead, names_session, needs_title, summary_title};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};

use crate::summary_port::parse_id;
use crate::{SqliteError, SqliteStore};

const OPENINGS: &str = "SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                              c.title_from_capture,
                              EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key),
                              o.content, o.title_armed
                         FROM ros_conversations c
                         JOIN rh_summary_openings o ON o.conversation_id = c.id
                        WHERE ?1 IS NULL OR c.id = ?1 ORDER BY c.id";

struct StoredHead {
    id: String,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    supplied: bool,
    trashed: bool,
}

impl StoredHead {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            session_key: row.get(1)?,
            agent: row.get(2)?,
            channel: row.get(3)?,
            title: row.get(4)?,
            supplied: row.get(5)?,
            trashed: row.get(6)?,
        })
    }

    fn head(self) -> Result<ConversationHead, SqliteError> {
        Ok(ConversationHead {
            id: parse_id(&self.id)?,
            session_key: self.session_key,
            agent: self.agent,
            channel: self.channel,
            title: self.title,
            trashed: self.trashed,
        })
    }
}

struct StoredOpening {
    head: StoredHead,
    content: String,
    armed: bool,
}

impl StoredOpening {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            head: StoredHead::read(row)?,
            content: row.get(7)?,
            armed: row.get(8)?,
        })
    }

    fn opening(self) -> Result<OpeningTitle, SqliteError> {
        let supplied = self.head.supplied;
        Ok(OpeningTitle {
            head: self.head.head()?,
            content: self.content,
            armed: self.armed,
            supplied,
        })
    }
}

impl SqliteStore {
    pub async fn backfill_titles(&self, dry_run: bool) -> Result<u64, SqliteError> {
        self.with_conn(move |conn| backfill(conn, dry_run)).await
    }
}

fn backfill(conn: &mut Connection, dry_run: bool) -> Result<u64, SqliteError> {
    let tx = conn.transaction().map_err(failure)?;
    let rows = {
        let mut stmt = tx.prepare(OPENINGS).map_err(failure)?;
        stmt.query_map([None::<&str>], StoredOpening::read)
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?
    };
    let mut titled = 0;
    for row in rows {
        let current = row.head.title.as_deref();
        if row.head.trashed || row.head.supplied || current.is_some_and(names_session) {
            continue;
        }
        if let Some(title) = summary_title(&row.content)
            && (dry_run || set_title(&tx, &row.head.id, current, &title)?)
        {
            titled += 1;
        }
    }
    tx.commit().map_err(failure)?;
    Ok(titled)
}

pub(crate) fn finish(
    conn: &mut Connection,
    opening: &OpeningTitle,
    title: Option<&str>,
) -> Result<bool, SqliteError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(failure)?;
    let id = opening.head.id.to_string();
    if load(&tx, &id)?.as_ref() != Some(opening) {
        return Ok(false);
    }
    if opening.armed {
        tx.execute(
            "UPDATE rh_summary_openings SET title_armed = 0 WHERE conversation_id = ?1",
            [&id],
        )
        .map_err(failure)?;
    }
    let changed = if let Some(title) = title.filter(|_| opening.wants_model()) {
        set_title(&tx, &id, opening.head.title.as_deref(), title)?
    } else {
        false
    };
    tx.commit().map_err(failure)?;
    Ok(changed)
}

pub(crate) fn load(conn: &Connection, id: &str) -> Result<Option<OpeningTitle>, SqliteError> {
    conn.query_row(OPENINGS, [id], StoredOpening::read)
        .optional()
        .map_err(failure)?
        .map(StoredOpening::opening)
        .transpose()
}

pub(crate) fn head(
    conn: &Connection,
    id: &str,
) -> Result<Option<(ConversationHead, bool)>, SqliteError> {
    let row = conn
        .query_row(
            "SELECT id, session_key, agent, channel, title, title_from_capture,
                    EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key)
           FROM ros_conversations c WHERE id = ?1",
            [id],
            StoredHead::read,
        )
        .optional()
        .map_err(failure)?;
    row.map(|row| {
        let supplied = row.supplied;
        Ok((row.head()?, supplied))
    })
    .transpose()
}

pub(crate) fn arm_historical(
    conn: &Connection,
    id: uuid::Uuid,
) -> Result<ArmOpeningOutcome, SqliteError> {
    let id = id.to_string();
    let Some((head, supplied)) = head(conn, &id)? else {
        return Ok(ArmOpeningOutcome::NotFound);
    };
    if let Some(reason) = Filable::exclusion_reason(&head) {
        return Ok(ArmOpeningOutcome::CannotFile(reason));
    }
    let has_opening = load(conn, &id)?.is_some();
    if !has_opening && !crate::summary_opening::seed_historical(conn, &id)? {
        return Ok(ArmOpeningOutcome::NoRequest);
    }
    let title = !supplied && needs_title(head.title.as_deref());
    conn.execute(
        "UPDATE rh_summary_openings SET historical_pending = 0, title_armed = ?2,
            filing_armed = NOT EXISTS (SELECT 1 FROM rh_conversation_placements WHERE conversation_id = ?1)
                AND NOT EXISTS (SELECT 1 FROM rh_session_placements WHERE session_id IN (?3, ?4))
         WHERE conversation_id = ?1 AND historical_pending",
         params![id, title, head.session_key, kernel::filing::session_key(Some(&head.channel), &head.session_key)],
    )
    .map_err(failure)?;
    Ok(ArmOpeningOutcome::Armed)
}

pub(crate) fn seed(
    conn: &Connection,
    head: &ConversationHead,
    old: Option<&OpeningTitle>,
    content: &str,
) -> Result<(), SqliteError> {
    let replace = match old {
        Some(old) => old.is_seed(),
        None => !head.title.as_deref().is_some_and(names_session),
    };
    if !head.trashed
        && replace
        && let Some(title) = summary_title(content)
    {
        set_title(conn, &head.id.to_string(), head.title.as_deref(), &title)?;
    }
    Ok(())
}

pub(crate) fn name_conversation(
    conn: &Connection,
    id: &str,
    title: &str,
) -> Result<bool, SqliteError> {
    let row = conn
        .query_row(
            "SELECT id, session_key, agent, channel, title, title_from_capture
           FROM ros_conversations WHERE id = ?1",
            [id],
            StoredHead::read,
        )
        .optional()
        .map_err(failure)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let current = row.title.as_deref();
    if row.supplied || !needs_title(current) {
        return Ok(false);
    }
    set_title(conn, id, current, title)
}

fn set_title(
    conn: &Connection,
    id: &str,
    current: Option<&str>,
    title: &str,
) -> Result<bool, SqliteError> {
    let updated = conn
        .execute(
            "UPDATE ros_conversations SET title = ?1
          WHERE id = ?2 AND title IS ?3 AND NOT title_from_capture",
            params![title, id, current],
        )
        .map_err(failure)?;
    Ok(updated == 1)
}

fn failure(source: rusqlite::Error) -> SqliteError {
    SqliteError::query("conversation opening title")(source)
}

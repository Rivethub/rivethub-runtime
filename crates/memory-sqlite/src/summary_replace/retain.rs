use std::collections::BTreeSet;

use kernel::memory::summary_reference::{
    PreparedSummaryTree, SectionIdentity, SummaryCoverage, SummaryReferenceError,
};
use rusqlite::{Connection, params};
use uuid::Uuid;

use super::publish::{failure, stored_uuid};
use crate::SqliteError;

pub(super) fn plan(
    conn: &Connection,
    tree: &mut PreparedSummaryTree,
    coverage: &mut SummaryCoverage,
) -> Result<BTreeSet<Uuid>, SqliteError> {
    let conversation = tree.conversation.to_string();
    let mut sources = conn
        .prepare(
            "SELECT message_id, ordinal, render_key FROM ros_summary_sources
         WHERE summary_id = ?1 ORDER BY ordinal",
        )
        .map_err(failure)?;
    for row in tree
        .records
        .iter()
        .filter(|row| tree.retained_parts.contains(&row.id))
    {
        let id = row.id.to_string();
        let valid: bool = conn
            .query_row(
                "SELECT EXISTS (
                SELECT 1 FROM ros_summaries
                WHERE id = ?1 AND conversation_id = ?2 AND kind = 'part'
                  AND input_key = ?3 AND content = ?4 AND issued_part_ordinal = ?5
                  AND summary_ordinal = ?6
            )",
                params![
                    id,
                    conversation,
                    row.input_key.as_slice(),
                    row.content,
                    row.issued_part_ordinal,
                    row.ordinal
                ],
                |r| r.get(0),
            )
            .map_err(failure)?;
        let stored = sources
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let mut expected: Vec<_> = row.sources.iter().collect();
        expected.sort_by_key(|source| source.ordinal);
        if !valid
            || stored
                .iter()
                .map(|(id, ordinal, key)| (id.clone(), *ordinal, key.as_deref()))
                .ne(expected.into_iter().map(|source| {
                    (
                        source.message.to_string(),
                        source.ordinal,
                        source.render_key.as_ref().map(<[_; 32]>::as_slice),
                    )
                }))
        {
            return Err(SummaryReferenceError::Stale(row.id).into());
        }
    }
    let mut sections = conn.prepare(
        "SELECT s.id, CASE WHEN p.kind = 'part' THEN p.id END, s.summary_ordinal, s.content,
                s.range_verified, (
                    SELECT json_group_array(message_id) FROM (
                        SELECT l.message_id FROM ros_summary_sources l
                        WHERE l.summary_id = p.id AND l.ordinal BETWEEN s.source_first AND s.source_last
                        ORDER BY l.ordinal
                    )
                )
         FROM ros_summaries s JOIN ros_summaries p ON p.id = s.parent_id
         WHERE s.conversation_id = ?1 AND s.kind = 'section'",
    ).map_err(failure)?;
    let rows = sections
        .query_map([conversation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(failure)?;
    let mut old = Vec::new();
    for row in rows {
        let (id, part, ordinal, content, verified, messages) = row.map_err(failure)?;
        let part = part.map(stored_uuid).transpose()?;
        if part.is_some_and(|id| !tree.retained_parts.contains(&id)) {
            continue;
        }
        let messages = serde_json::from_str(&messages).map_err(|_| SqliteError::StoredValue {
            column: "section sources",
            value: messages,
        })?;
        old.push((
            stored_uuid(id)?,
            SectionIdentity {
                part,
                ordinal,
                content,
                verified,
                messages,
            },
        ));
    }
    let sections = tree.retain_sections(coverage, old);
    Ok(sections.union(&tree.retained_parts).copied().collect())
}

pub(super) fn reparent(
    conn: &Connection,
    tree: &PreparedSummaryTree,
    retained: &BTreeSet<Uuid>,
) -> Result<(), SqliteError> {
    conn.execute_batch("PRAGMA defer_foreign_keys = ON")
        .map_err(failure)?;
    for row in tree.records.iter().filter(|row| retained.contains(&row.id)) {
        let id = row.id.to_string();
        let parent = row.parent.map(|id| id.to_string());
        if let Some(bounds) = &row.section {
            conn.execute(
                "UPDATE ros_summaries SET parent_id = ?2, source_first = ?3, source_last = ?4,
                    section_char_start = ?5, section_char_end = ?6, input_key = ?7, quality_flags = ?8
                 WHERE id = ?1",
                params![id, parent, bounds.first, bounds.last, bounds.char_start, bounds.char_end,
                    row.input_key.as_slice(), row.quality_flags],
            ).map_err(failure)?;
        } else {
            conn.execute(
                "UPDATE ros_summaries SET parent_id = ?2 WHERE id = ?1",
                params![id, parent],
            )
            .map_err(failure)?;
        }
    }
    Ok(())
}

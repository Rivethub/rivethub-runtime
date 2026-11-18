use std::collections::BTreeSet;

use kernel::memory::{
    SummaryKind,
    summary_reference::{
        PreparedSummaryRecord, PreparedSummaryTree, SectionIdentity, SummaryCoverage,
        SummaryReferenceError,
    },
};
use sqlx::PgConnection;
use uuid::Uuid;

use super::publish::{ReplaceSummaryError, query};
use crate::Error;

pub(super) async fn plan(
    conn: &mut PgConnection,
    tree: &mut PreparedSummaryTree,
    coverage: &mut SummaryCoverage,
) -> Result<BTreeSet<Uuid>, ReplaceSummaryError> {
    for row in tree
        .records
        .iter()
        .filter(|row| tree.retained_parts.contains(&row.id))
    {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM rh_summaries
                WHERE id = $1 AND conversation_id = $2 AND owner_user_id = $3 AND kind = 'part'
                  AND input_key = $4 AND content = $5 AND issued_part_ordinal = $6
            )",
        )
        .bind(row.id)
        .bind(tree.conversation)
        .bind(&tree.owner)
        .bind(row.input_key.as_slice())
        .bind(&row.content)
        .bind(row.issued_part_ordinal)
        .fetch_one(&mut *conn)
        .await
        .map_err(query)?;
        let stored: Vec<(Uuid, i32, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT message_id, ordinal, render_key FROM rh_summary_sources
             WHERE summary_id = $1 ORDER BY ordinal",
        )
        .bind(row.id)
        .fetch_all(&mut *conn)
        .await
        .map_err(query)?;
        let mut expected: Vec<_> = row.sources.iter().collect();
        expected.sort_by_key(|source| source.ordinal);
        if !valid
            || stored
                .iter()
                .map(|(id, ordinal, key)| (*id, *ordinal, key.as_deref()))
                .ne(expected.into_iter().map(|source| {
                    (
                        source.message,
                        source.ordinal,
                        source.render_key.as_ref().map(<[_; 32]>::as_slice),
                    )
                }))
        {
            return Err(SummaryReferenceError::Stale(row.id).into());
        }
    }
    let parts: Vec<_> = tree.retained_parts.iter().copied().collect();
    let old: Vec<(Uuid, Option<Uuid>, i32, String, bool, Vec<Uuid>)> = sqlx::query_as(
        "SELECT s.id, CASE WHEN p.kind = 'part' THEN p.id END, s.summary_ordinal, s.content,
                s.range_verified, ARRAY(
                    SELECT l.message_id FROM rh_summary_sources l
                    WHERE l.summary_id = p.id AND l.ordinal BETWEEN s.source_first AND s.source_last
                    ORDER BY l.ordinal
                )
         FROM rh_summaries s JOIN rh_summaries p ON p.id = s.parent_id
         WHERE s.conversation_id = $1 AND s.owner_user_id = $2 AND s.kind = 'section'
           AND (p.kind = 'session' OR p.id = ANY($3))",
    )
    .bind(tree.conversation)
    .bind(&tree.owner)
    .bind(&parts)
    .fetch_all(conn)
    .await
    .map_err(query)?;
    let sections = tree.retain_sections(
        coverage,
        old.into_iter()
            .map(|(id, part, ordinal, content, verified, messages)| {
                (
                    id,
                    SectionIdentity {
                        part,
                        ordinal,
                        content,
                        verified,
                        messages,
                    },
                )
            }),
    );
    Ok(sections.union(&tree.retained_parts).copied().collect())
}

pub(super) async fn reparent(
    conn: &mut PgConnection,
    row: &PreparedSummaryRecord,
) -> Result<(), Error> {
    if row.kind == SummaryKind::Part {
        sqlx::query("UPDATE rh_summaries SET parent_id = $2 WHERE id = $1")
            .bind(row.id)
            .bind(row.parent)
            .execute(conn)
            .await
            .map_err(query)?;
    } else if let Some(bounds) = &row.section {
        sqlx::query(
            "UPDATE rh_summaries SET parent_id = $2, source_first = $3, source_last = $4,
                section_char_start = $5, section_char_end = $6, input_key = $7, quality_flags = $8
             WHERE id = $1",
        )
        .bind(row.id)
        .bind(row.parent)
        .bind(bounds.first)
        .bind(bounds.last)
        .bind(bounds.char_start)
        .bind(bounds.char_end)
        .bind(row.input_key.as_slice())
        .bind(row.quality_flags)
        .execute(conn)
        .await
        .map_err(query)?;
    }
    Ok(())
}

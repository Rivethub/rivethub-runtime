use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;

use kernel::memory::{TagEntityType, TagLiteral};
use rusqlite::Connection;

use crate::SqliteError;

const TAGS_SQL: &str =
    "SELECT entity_type, entity_id, key, value FROM ros_tags WHERE state = 'accepted'";

const SUMMARY_LINKS_SQL: &str = "SELECT id, conversation_id FROM ros_summaries";

#[derive(Debug, Default)]
pub(super) struct TagIndex {
    conversation: BTreeMap<String, BTreeSet<(String, String)>>,
    summary: BTreeMap<String, BTreeSet<(String, String)>>,
    summaries_of: BTreeMap<String, Vec<String>>,
}

impl TagIndex {
    pub(super) fn load(conn: &Connection) -> Result<Self, SqliteError> {
        let mut index = Self::default();
        index.read_tags(conn)?;
        index.read_links(conn)?;
        Ok(index)
    }

    fn read_tags(&mut self, conn: &Connection) -> Result<(), SqliteError> {
        let rows = collect_tags(conn)?;
        for (entity_type, entity_id, key, value) in rows {
            self.insert_tag(entity_type, entity_id, key, value)?;
        }
        Ok(())
    }

    fn insert_tag(
        &mut self,
        entity_type: String,
        entity_id: String,
        key: String,
        value: String,
    ) -> Result<(), SqliteError> {
        let kind = TagEntityType::from_str(&entity_type)
            .map_err(|_| SqliteError::StoredTagType { value: entity_type })?;
        let pairs = match kind {
            TagEntityType::Conversation => &mut self.conversation,
            TagEntityType::Summary => &mut self.summary,
        };
        pairs.entry(entity_id).or_default().insert((key, value));
        Ok(())
    }

    fn read_links(&mut self, conn: &Connection) -> Result<(), SqliteError> {
        for (summary_id, conversation_id) in collect_links(conn)? {
            if let Some(conversation_id) = conversation_id {
                self.summaries_of
                    .entry(conversation_id)
                    .or_default()
                    .push(summary_id);
            }
        }
        Ok(())
    }

    pub(super) fn message_has(&self, conversation_id: &str, tag: &TagLiteral) -> bool {
        pair_has(&self.conversation, conversation_id, tag)
            || self
                .summaries_of
                .get(conversation_id)
                .is_some_and(|ids| ids.iter().any(|id| pair_has(&self.summary, id, tag)))
    }

    pub(super) fn summary_has(&self, id: &str, tag: &TagLiteral) -> bool {
        pair_has(&self.summary, id, tag)
    }
}

fn pair_has(
    map: &BTreeMap<String, BTreeSet<(String, String)>>,
    id: &str,
    tag: &TagLiteral,
) -> bool {
    map.get(id).is_some_and(|pairs| {
        pairs
            .iter()
            .any(|(key, value)| key == &tag.key && value == &tag.value)
    })
}

fn collect_tags(conn: &Connection) -> Result<Vec<(String, String, String, String)>, SqliteError> {
    let mut statement = conn
        .prepare(TAGS_SQL)
        .map_err(SqliteError::query("search tags"))?;
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .map_err(SqliteError::query("search tags"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("search tags"))
}

fn collect_links(conn: &Connection) -> Result<Vec<(String, Option<String>)>, SqliteError> {
    let mut statement = conn
        .prepare(SUMMARY_LINKS_SQL)
        .map_err(SqliteError::query("search summary links"))?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(SqliteError::query("search summary links"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("search summary links"))
}

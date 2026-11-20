use rusqlite::{OptionalExtension as _, Transaction, params};

use super::PreparedMessage;
use crate::SqliteError;
use wire::message::Role;

impl PreparedMessage {
    pub(super) fn stored_by_event(
        &self,
        tx: &Transaction<'_>,
        conversation_id: &str,
    ) -> Result<Option<String>, SqliteError> {
        tx.query_row(
            "SELECT id FROM ros_messages
              WHERE conversation_id = ?1 AND (
                json_extract(metadata, '$.event_id') = ?2
                OR json_extract(metadata, '$.cowork_transcript_event_id') = ?2
              )
              ORDER BY rowid LIMIT 1",
            params![conversation_id, self.event_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("event id lookup"))
    }

    pub(super) fn fill_pending(&self, tx: &Transaction<'_>, id: &str) -> Result<bool, SqliteError> {
        if self.role != Role::Tool || self.tool_result.as_deref().is_none_or(str::is_empty) {
            return Ok(false);
        }
        let changed = tx
            .execute(
                "UPDATE ros_messages
                    SET tool_result = ?2, metadata = json_set(metadata, '$.pending_result', json('false'))
                  WHERE id = ?1 AND role = 'tool' AND nullif(tool_result, '') IS NULL
                    AND json_extract(metadata, '$.pending_result') = 1
                    AND json_extract(?3, '$.pending_result') IS NOT 1",
                params![id, self.tool_result, self.metadata],
            )
            .map_err(SqliteError::query("pending tool result fill"))?;
        Ok(changed > 0)
    }

    pub(super) fn fill_position(
        &self,
        tx: &Transaction<'_>,
        id: &str,
    ) -> Result<bool, SqliteError> {
        let Some(patch) = &self.position_patch else {
            return Ok(false);
        };
        let changed = tx
            .execute(
                "UPDATE ros_messages SET metadata = json_patch(metadata, (
                 SELECT json_group_object(field.key, ?2 -> field.key)
                   FROM json_each(?2) AS field
                  WHERE json_type(metadata, '$.' || field.key) IS NULL
             ))
             WHERE id = ?1 AND EXISTS (
                 SELECT 1 FROM json_each(?2) AS field
                  WHERE json_type(metadata, '$.' || field.key) IS NULL
             )",
                params![id, patch],
            )
            .map_err(SqliteError::query("transcript position fill"))?;
        Ok(changed > 0)
    }

    pub(super) fn unclaimed_hook_row(
        &self,
        tx: &Transaction<'_>,
        conversation_id: &str,
    ) -> Result<Option<String>, SqliteError> {
        tx.query_row(
            "SELECT id FROM ros_messages
              WHERE json_extract(?4, '$.source') = 'cowork-transcript'
                AND conversation_id = ?1
                AND json_extract(metadata, '$.source') = 'cowork-hook'
                AND json_type(metadata, '$.cowork_transcript_event_id') IS NULL
                AND role = ?2 AND content = ?3
              ORDER BY rowid LIMIT 1",
            params![
                conversation_id,
                self.role.to_string(),
                self.content,
                self.metadata
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("cowork hook lookup"))
    }
}

use std::path::Path;

use kernel::transcript::StoreSession;
use kernel::transcript::TranscriptReaderError;
use rusqlite::types::Value as SqlValue;
use serde::Deserialize;
use wire::harness::HarnessId;

use super::field::Field;
use super::field::text_of;
use super::field::typed;
use super::text::clip;
use super::text::collapse;
use super::text::non_empty;
use super::time::RawTime;
use super::time::epoch_flex;
use super::time::epoch_opencode;
use crate::readers::harness_db::DbRows;
use crate::readers::harness_db::HermesSessionRow;
use crate::readers::harness_db::OpencodeSessionRow;
use crate::readers::harness_db::SessionQuery;
use crate::readers::harness_db::hermes_session_rows;
use crate::readers::harness_db::opencode_session_rows;
use crate::readers::harness_db::read_error;
use crate::readers::sqlite::database_file;

const HERMES_FILE: &str = "state.db";
const OPENCODE_FILE: &str = "opencode.db";
const TITLE_CHARS: usize = 120;

pub(crate) fn hermes_list(
    root: &Path,
    limit: usize,
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    Ok(hermes_rows(root, SessionQuery::Recent(limit))?
        .iter()
        .filter_map(|row| hermes_row(row, false))
        .collect())
}

pub(crate) fn hermes_describe(
    root: &Path,
    id: &str,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    Ok(hermes_rows(root, SessionQuery::One(id))?
        .iter()
        .find_map(|row| hermes_row(row, true)))
}

fn hermes_rows(
    root: &Path,
    query: SessionQuery<'_>,
) -> Result<Vec<HermesSessionRow>, TranscriptReaderError> {
    let Some(path) = database_file(root, HERMES_FILE)? else {
        return Ok(Vec::new());
    };
    match hermes_session_rows(&path, query).map_err(read_error)? {
        DbRows::Ready(rows) => Ok(rows),
        DbRows::Unreadable => Ok(Vec::new()),
    }
}

fn sql_text_id(value: &SqlValue) -> Option<String> {
    match value {
        SqlValue::Text(text) => Some(text.clone()),
        SqlValue::Integer(number) => Some(number.to_string()),
        SqlValue::Real(number) => Some(number.to_string()),
        SqlValue::Null | SqlValue::Blob(_) => None,
    }
}

fn hermes_row(row: &HermesSessionRow, describing: bool) -> Option<StoreSession> {
    let id = sql_text_id(&row.id)?;
    let finished = if row.ended_at == SqlValue::Null {
        &row.started_at
    } else {
        &row.ended_at
    };
    let mut session = StoreSession::new(
        HarnessId::Hermes,
        &id,
        epoch_flex(&RawTime::from_sql(finished)),
    );
    if let SqlValue::Text(title) = &row.title
        && let Some(title) = non_empty(clip(title, TITLE_CHARS))
    {
        session.title = title;
    }
    if describing {
        session.created_at =
            Some(epoch_flex(&RawTime::from_sql(&row.started_at))).filter(|ms| *ms != 0);
    }
    Some(session)
}

pub(crate) fn opencode_list(
    root: &Path,
    limit: usize,
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    Ok(opencode_rows(root, SessionQuery::Recent(limit))?
        .iter()
        .filter_map(opencode_row)
        .collect())
}

pub(crate) fn opencode_describe(
    root: &Path,
    id: &str,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return Ok(None);
    }
    Ok(opencode_rows(root, SessionQuery::One(id))?
        .iter()
        .find_map(opencode_row))
}

fn opencode_rows(
    root: &Path,
    query: SessionQuery<'_>,
) -> Result<Vec<OpencodeSessionRow>, TranscriptReaderError> {
    let Some(path) = database_file(root, OPENCODE_FILE)? else {
        return Ok(Vec::new());
    };
    match opencode_session_rows(&path, query).map_err(read_error)? {
        DbRows::Ready(rows) => Ok(rows),
        DbRows::Unreadable => Ok(Vec::new()),
    }
}

fn opencode_row(row: &OpencodeSessionRow) -> Option<StoreSession> {
    let SqlValue::Text(id) = &row.id else {
        return None;
    };
    let created = epoch_opencode(&RawTime::from_sql(&row.time_created));
    let updated = Some(epoch_opencode(&RawTime::from_sql(&row.time_updated)))
        .filter(|ms| *ms != 0)
        .unwrap_or(created);
    let mut session = StoreSession::new(HarnessId::Opencode, id, updated);
    if let SqlValue::Text(title) = &row.title
        && let Some(title) = non_empty(collapse(title).chars().take(TITLE_CHARS).collect())
    {
        session.title = title;
    }
    session.created_at = Some(created).filter(|ms| *ms != 0);
    session.model = model_label(&row.model);
    if let SqlValue::Text(parent) = &row.parent_id
        && !parent.is_empty()
    {
        session.parent_session_id = Some(parent.clone());
    }
    Some(session)
}

#[derive(Debug, Deserialize)]
struct ModelRef {
    #[serde(rename = "providerID")]
    provider_id: Option<Field>,
    id: Option<Field>,
}

pub(crate) fn model_label(raw: &SqlValue) -> Option<String> {
    let SqlValue::Text(text) = raw else {
        return None;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some(parsed) = typed::<Field>(trimmed) else {
        return Some(trimmed.to_owned());
    };
    let model = parsed.object::<ModelRef>()?;
    let id = text_of(model.id.as_ref()).filter(|id| !id.is_empty());
    match (
        text_of(model.provider_id.as_ref()).filter(|provider| !provider.is_empty()),
        id,
    ) {
        (Some(provider), Some(id)) => Some(format!("{provider}/{id}")),
        (_, id) => id,
    }
}

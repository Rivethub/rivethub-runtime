use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipRule {
    UnknownRecordType,
    UnparseableRecord,
    UnparseableTimestamp,
    UnrecognizedSpeaker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRecord {
    pub position: u64,
    pub rule: SkipRule,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSlice {
    pub messages: Vec<CaptureMessage>,
    pub next_offset: u64,
    pub skipped: Vec<SkippedRecord>,
}

impl TranscriptSlice {
    #[must_use]
    pub fn empty(offset: u64) -> Self {
        Self {
            messages: Vec::new(),
            next_offset: offset,
            skipped: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Draft {
    pub position: u64,
    pub message: CaptureMessage,
}

#[derive(Debug)]
pub(crate) enum Mapped {
    Draft(Box<Draft>),
    Skip(SkippedRecord),
    None,
}

impl Mapped {
    pub(crate) fn with_metadata(
        mut self,
        add: impl FnOnce(&mut JsonObject) -> Result<(), serde_json::Error>,
    ) -> Result<Self, TranscriptReaderError> {
        if let Self::Draft(draft) = &mut self
            && let Some(metadata) = &mut draft.message.metadata
        {
            add(metadata).map_err(json_failed)?;
        }
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ToolFields {
    pub name: Option<String>,
    pub arguments: Option<JsonObject>,
    pub result: Option<String>,
}

impl ToolFields {
    #[must_use]
    pub(crate) fn none() -> Self {
        Self {
            name: None,
            arguments: None,
            result: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TextTurn<'a> {
    pub session: &'a SessionId,
    pub position: u64,
    pub role: Role,
    pub content: &'a str,
    pub at: &'a str,
    pub format: &'static str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TypeTag {
    #[serde(rename = "type")]
    pub record_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StoreRole {
    User,
    Assistant,
    System,
    Tool,
    #[serde(other)]
    Other,
}

impl StoreRole {
    #[must_use]
    pub(crate) fn known(self) -> Option<Role> {
        match self {
            Self::User => Some(Role::User),
            Self::Assistant => Some(Role::Assistant),
            Self::System => Some(Role::System),
            Self::Tool => Some(Role::Tool),
            Self::Other => None,
        }
    }
}

pub(crate) fn line_number(number: usize) -> Result<u64, TranscriptReaderError> {
    u64::try_from(number).map_err(|error| TranscriptReaderError::Read {
        message: format!("transcript line number does not fit: {error}"),
        source: Box::new(error),
    })
}

pub(crate) fn row_offset(offset: u64) -> Result<i64, TranscriptReaderError> {
    i64::try_from(offset).map_err(|error| TranscriptReaderError::Read {
        message: format!("transcript offset does not fit a sqlite row id: {error}"),
        source: Box::new(error),
    })
}

pub(crate) fn row_position(row_id: i64) -> Result<u64, TranscriptReaderError> {
    u64::try_from(row_id).map_err(|error| TranscriptReaderError::Read {
        message: format!("transcript row id does not fit: {error}"),
        source: Box::new(error),
    })
}

#[must_use]
pub(crate) fn native(session: &SessionId, harness: HarnessId) -> Option<&str> {
    (session.harness() == harness).then(|| session.native())
}

pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TranscriptReaderError> + Send + 'static,
) -> Result<T, TranscriptReaderError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| TranscriptReaderError::Unavailable(Box::new(error)))?
}

pub(crate) fn report(format: &str, skipped: &[SkippedRecord]) {
    if skipped.is_empty() {
        return;
    }
    let positions: Vec<u64> = skipped.iter().map(|record| record.position).collect();
    tracing::debug!(
        store_format = format,
        count = skipped.len(),
        ?positions,
        "transcript records were skipped"
    );
}

pub(crate) fn project_turns(slice: &TranscriptSlice) -> Vec<Turn> {
    let mut turns = Vec::new();
    for message in &slice.messages {
        let Some(at) = message.created_at else {
            continue;
        };
        turns.push(Turn {
            role: message.role,
            content: message.content.clone(),
            at,
        });
    }
    turns
}

pub(crate) fn assemble(
    offset: u64,
    last: u64,
    drafts: Vec<Draft>,
    mut skipped: Vec<SkippedRecord>,
) -> TranscriptSlice {
    skipped.retain(|record| record.position > offset);
    let messages = drafts
        .into_iter()
        .filter(|draft| draft.position > offset)
        .map(|draft| draft.message)
        .collect();
    TranscriptSlice {
        messages,
        next_offset: last.max(offset),
        skipped,
    }
}

pub(crate) fn text_turn(turn: &TextTurn<'_>) -> Result<Mapped, TranscriptReaderError> {
    if turn.content.trim().is_empty() {
        return Ok(Mapped::None);
    }
    match DateTime::parse_from_rfc3339(turn.at) {
        Ok(_) => {}
        Err(error) => {
            return Ok(Mapped::Skip(SkippedRecord {
                position: turn.position,
                rule: SkipRule::UnparseableTimestamp,
                detail: format!("timestamp {:?} is not RFC 3339: {error}", turn.at),
            }));
        }
    }
    Ok(Mapped::Draft(Box::new(Draft {
        position: turn.position,
        message: capture(turn, ToolFields::none())?,
    })))
}

pub(crate) fn tool_turn(
    turn: &TextTurn<'_>,
    tool: ToolFields,
) -> Result<Mapped, TranscriptReaderError> {
    if turn.content.is_empty() {
        return Ok(Mapped::None);
    }
    match DateTime::parse_from_rfc3339(turn.at) {
        Ok(_) => {}
        Err(error) => {
            return Ok(Mapped::Skip(SkippedRecord {
                position: turn.position,
                rule: SkipRule::UnparseableTimestamp,
                detail: format!("timestamp {:?} is not RFC 3339: {error}", turn.at),
            }));
        }
    }
    Ok(Mapped::Draft(Box::new(Draft {
        position: turn.position,
        message: capture(turn, tool)?,
    })))
}

pub(crate) fn push_mapped(
    mapped: Mapped,
    drafts: &mut Vec<Draft>,
    skipped: &mut Vec<SkippedRecord>,
) {
    match mapped {
        Mapped::Draft(draft) => drafts.push(*draft),
        Mapped::Skip(record) => skipped.push(record),
        Mapped::None => {}
    }
}

pub(crate) fn unknown(position: u64, detail: impl Into<String>) -> SkippedRecord {
    SkippedRecord {
        position,
        rule: SkipRule::UnknownRecordType,
        detail: detail.into(),
    }
}

pub(crate) fn unparseable(position: u64, detail: impl Into<String>) -> SkippedRecord {
    SkippedRecord {
        position,
        rule: SkipRule::UnparseableRecord,
        detail: detail.into(),
    }
}

pub(crate) fn parse_json<T: for<'de> Deserialize<'de>>(
    position: u64,
    text: &str,
) -> Result<T, SkippedRecord> {
    serde_json::from_str(text).map_err(|error| unparseable(position, error.to_string()))
}

pub(crate) fn arguments(position: u64, text: &str) -> Result<JsonObject, SkippedRecord> {
    parse_json(position, text)
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(transparent, bound = "T: Deserialize<'de>")]
struct Lenient<T>(#[serde_as(as = "DefaultOnError")] Option<T>);

#[derive(Debug, Serialize)]
struct Wrapped<'a> {
    input: &'a RawValue,
}

pub(crate) fn lenient<T: for<'de> Deserialize<'de>>(
    raw: &RawValue,
) -> Result<Option<T>, TranscriptReaderError> {
    serde_json::from_str::<Lenient<T>>(raw.get())
        .map(|lenient| lenient.0)
        .map_err(json_failed)
}

pub(crate) fn tool_input(raw: &RawValue) -> Result<JsonObject, TranscriptReaderError> {
    if let Some(object) = lenient::<JsonObject>(raw)? {
        return Ok(object);
    }
    if let Some(text) = lenient::<String>(raw)? {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let parsed = serde_json::from_str::<JsonObject>(&text).ok();
        if let Some(object) = parsed {
            return Ok(object);
        }
    }
    JsonObject::from_serialize(&Wrapped { input: raw }).map_err(json_failed)
}

pub(crate) fn json_failed(error: serde_json::Error) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        message: format!("capture JSON failed: {error}"),
        source: Box::new(error),
    }
}

#[derive(Serialize)]
struct RecordMeta<'a> {
    store_format: &'a str,
    position: u64,
}

fn capture(turn: &TextTurn<'_>, tool: ToolFields) -> Result<CaptureMessage, TranscriptReaderError> {
    let metadata = JsonObject::from_serialize(&RecordMeta {
        store_format: turn.format,
        position: turn.position,
    })
    .map_err(|error| TranscriptReaderError::Read {
        message: format!("transcript metadata encoding failed: {error}"),
        source: Box::new(error),
    })?;
    Ok(CaptureMessage {
        event_id: format!("{}:l{}", turn.session, turn.position),
        role: turn.role,
        content: turn.content.to_owned(),
        tool_name: tool.name,
        tool_args: tool.arguments,
        tool_result: tool.result,
        metadata: Some(metadata),
        created_at: Some(message_instant(turn.at)?),
    })
}

fn message_instant(raw: &str) -> Result<DateTime<Utc>, TranscriptReaderError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| TranscriptReaderError::Read {
            message: format!("transcript timestamp {raw:?} is not RFC 3339: {error}"),
            source: Box::new(error),
        })
}

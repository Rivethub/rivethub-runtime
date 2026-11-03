mod output;

use std::path::Path;

use kernel::transcript::HarnessTranscriptTool;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::MessageUsage;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::StoreTranscript;
use kernel::transcript::TranscriptAttachment;
use kernel::transcript::TranscriptBlock;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use serde::Deserialize;
use wire::JsonObject;

use super::super::claude::transcript_file;
use super::super::field::Field;
use super::super::field::object_line;
use super::super::field::text_of;
use super::super::files::Span;
use super::super::files::read_failure;
use super::super::files::read_span;
use super::super::text::human_turn;
use super::super::text::strip_paste;
use super::super::text::text_elements;
use super::super::time::parse_date;
use super::display::tail_chars;
use super::display::token_label;
use super::tools::RESULT_CHARS;
use super::tools::args_summary;
use super::tools::is_prompt_tool;
use super::tools::keeps_input;
use super::tools::kept_input;

pub(crate) use output::tool_output;

pub(crate) fn transcript(
    root: &Path,
    id: &str,
    wanted: TranscriptWindow,
) -> Result<StoreTranscript, TranscriptReaderError> {
    let Some(path) = transcript_file(root, id)? else {
        return Ok(StoreTranscript::default());
    };
    let failed = |error| read_failure(&path, error);
    let mut span = Span::of(&path, wanted).map_err(failed)?;
    loop {
        let window = read_span(&path, span).map_err(failed)?;
        let mut records = Vec::new();
        let mut offsets = Vec::new();
        let mut at = window.start;
        for line in window.text.split_inclusive('\n') {
            if let Some(record) = object_line::<Record>(line) {
                records.push(record);
                offsets.push(at);
            }
            at += line.len() as u64;
        }
        let sidechains = agent_file(&path) || (window.start == 0 && all_sidechain(&records));
        let first = if window.start == 0 || span.at_line {
            Some(0)
        } else {
            records
                .iter()
                .position(|record| opens_turn(record, sidechains))
        };
        if let Some(first) = first {
            let start = offsets.get(first).copied().unwrap_or(window.start);
            let (turns, context) = fold(records.get(first..).unwrap_or_default(), sidechains);
            return Ok(StoreTranscript {
                turns,
                earlier: (start > 0).then_some(start),
                context,
            });
        }
        span = span.widened();
    }
}

fn opens_turn(record: &Record, sidechains: bool) -> bool {
    record.kind() == Kind::User
        && (sidechains || !record.sidechain())
        && !record.skipped()
        && record
            .body()
            .and_then(|body| body.content.as_ref().and_then(user_text))
            .is_some()
}

fn agent_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("agent-"))
}

fn all_sidechain(records: &[Record]) -> bool {
    records
        .iter()
        .filter(|record| matches!(record.kind(), Kind::User | Kind::Assistant))
        .all(Record::sidechain)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type")]
    kind: Option<Field>,
    subtype: Option<Field>,
    is_sidechain: Option<Field>,
    is_meta: Option<Field>,
    is_compact_summary: Option<Field>,
    compact_metadata: Option<Field>,
    message: Option<Field>,
    tool_use_result: Option<Field>,
    timestamp: Option<Field>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Assistant,
    CompactBoundary,
    Other,
}

fn flagged(field: Option<&Field>) -> bool {
    field.is_some_and(Field::flag)
}

impl Record {
    fn kind(&self) -> Kind {
        match text_of(self.kind.as_ref()).as_deref() {
            Some("user") => Kind::User,
            Some("assistant") => Kind::Assistant,
            Some("system")
                if self
                    .subtype
                    .as_ref()
                    .is_some_and(|kind| kind.is_text("compact_boundary")) =>
            {
                Kind::CompactBoundary
            }
            Some(_) | None => Kind::Other,
        }
    }

    fn sidechain(&self) -> bool {
        flagged(self.is_sidechain.as_ref())
    }

    fn skipped(&self) -> bool {
        flagged(self.is_meta.as_ref()) || flagged(self.is_compact_summary.as_ref())
    }

    fn body(&self) -> Option<Body> {
        self.message.as_ref().and_then(Field::object::<Body>)
    }

    fn at_ms(&self) -> Option<i64> {
        text_of(self.timestamp.as_ref())
            .as_deref()
            .and_then(parse_date)
    }
}

#[derive(Debug, Deserialize)]
struct Body {
    id: Option<Field>,
    content: Option<Field>,
    stop_reason: Option<Field>,
    usage: Option<Field>,
    model: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<Field>,
    text: Option<Field>,
    thinking: Option<Field>,
    name: Option<Field>,
    id: Option<Field>,
    input: Option<Field>,
    tool_use_id: Option<Field>,
    is_error: Option<Field>,
    content: Option<Field>,
    source: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct ImageSource {
    media_type: Option<Field>,
}

fn image_attachment(block: &Block) -> Option<TranscriptAttachment> {
    let source = block.source.as_ref()?.object::<ImageSource>()?;
    Some(TranscriptAttachment {
        media_type: text_of(source.media_type.as_ref())?,
        name: None,
        uri: None,
        upload_id: None,
    })
}

impl Block {
    fn is(&self, kind: &str) -> bool {
        self.kind.as_ref().is_some_and(|field| field.is_text(kind))
    }
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(rename = "input_tokens")]
    input: Option<Field>,
    #[serde(rename = "cache_read_input_tokens")]
    cache_read: Option<Field>,
    #[serde(rename = "cache_creation_input_tokens")]
    cache_creation: Option<Field>,
    #[serde(rename = "output_tokens")]
    output: Option<Field>,
}

fn tokens(field: Option<&Field>) -> u64 {
    field.and_then(Field::count).unwrap_or_default()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactMetadata {
    pre_tokens: Option<Field>,
    post_tokens: Option<Field>,
}

#[derive(Debug, Default)]
struct Draft {
    text: String,
    thinking: String,
    tools: Vec<HarnessTranscriptTool>,
    usage: Option<MessageUsage>,
    output: u64,
    call: Option<String>,
    context: u64,
    model: Option<String>,
    stop_reason: Option<String>,
    last_block: Option<TranscriptBlock>,
    last_line_had_text: bool,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
}

impl Draft {
    fn complete(&self) -> bool {
        self.stop_reason.as_deref() == Some("end_turn")
            && self.last_line_had_text
            && !self
                .tools
                .iter()
                .any(|tool| tool.status == TranscriptToolStatus::Running)
    }

    fn close(self) -> Option<HarnessTranscriptTurn> {
        let complete = self.complete();
        if self.text.is_empty() && self.thinking.is_empty() && self.tools.is_empty() {
            return None;
        }
        let thinking = (!self.thinking.is_empty()).then(|| tail_chars(&self.thinking));
        let output = self.output;
        Some(HarnessTranscriptTurn {
            role: SessionMessageRole::Assistant,
            text: self.text,
            thinking,
            tools: (!self.tools.is_empty()).then_some(self.tools),
            usage: self.usage.map(|usage| MessageUsage {
                completion_tokens: output,
                ..usage
            }),
            model: self.model,
            stop_reason: self.stop_reason,
            last_block: self.last_block,
            attachments: None,
            complete,
            compact: false,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
        })
    }
}

#[derive(Debug, Default)]
struct Folder {
    turns: Vec<HarnessTranscriptTurn>,
    pending: Option<Draft>,
    context: Option<u64>,
}

fn fold(records: &[Record], sidechains: bool) -> (Vec<HarnessTranscriptTurn>, Option<u64>) {
    let mut folder = Folder::default();
    for record in records {
        if (record.sidechain() && !sidechains) || record.skipped() {
            continue;
        }
        match record.kind() {
            Kind::CompactBoundary => folder.boundary(record),
            Kind::User => folder.user(record),
            Kind::Assistant => folder.assistant(record),
            Kind::Other => {}
        }
    }
    folder.flush();
    (folder.turns, folder.context)
}

impl Folder {
    fn flush(&mut self) {
        if let Some(pending) = self.pending.as_ref().filter(|pending| pending.context > 0) {
            self.context = Some(pending.context);
        }
        if let Some(turn) = self.pending.take().and_then(Draft::close) {
            self.turns.push(turn);
        }
    }

    fn boundary(&mut self, record: &Record) {
        match &self.pending {
            Some(pending) if !pending.complete() => return,
            None if self
                .turns
                .last()
                .is_some_and(|turn| turn.role == SessionMessageRole::User) =>
            {
                return;
            }
            Some(_) | None => {}
        }
        self.flush();
        self.turns.push(compact_marker(record));
    }

    fn user(&mut self, record: &Record) {
        let Some(body) = record.body() else {
            return;
        };
        let blocks = body
            .content
            .as_ref()
            .and_then(Field::items)
            .unwrap_or_default();
        let mut images = Vec::new();
        for block in blocks.iter().filter_map(Field::object::<Block>) {
            if block.is("tool_result") {
                self.tool_result(record, &block);
            } else if block.is("image")
                && let Some(image) = image_attachment(&block)
            {
                images.push(image);
            }
        }
        let text = match body.content.as_ref().and_then(user_text) {
            Some(text) => text,
            None if !images.is_empty()
                && text_elements(&blocks)
                    .iter()
                    .all(|text| text.trim().is_empty()) =>
            {
                String::new()
            }
            None => return,
        };
        self.flush();
        let at = record.at_ms();
        self.turns.push(HarnessTranscriptTurn {
            role: SessionMessageRole::User,
            text,
            thinking: None,
            tools: None,
            usage: None,
            model: None,
            stop_reason: None,
            last_block: None,
            attachments: (!images.is_empty()).then_some(images),
            complete: false,
            compact: false,
            started_at_ms: at,
            ended_at_ms: at,
        });
    }

    fn tool_result(&mut self, record: &Record, block: &Block) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        pending.last_block = Some(TranscriptBlock::ToolResult);
        pending.stamp(record.at_ms());
        let Some(id) = text_of(block.tool_use_id.as_ref()) else {
            return;
        };
        let Some(tool) = pending
            .tools
            .iter_mut()
            .find(|tool| tool.id.as_deref() == Some(id.as_str()))
        else {
            return;
        };
        tool.status = if flagged(block.is_error.as_ref()) {
            TranscriptToolStatus::Error
        } else {
            TranscriptToolStatus::Done
        };
        if is_prompt_tool(&tool.name) {
            tool.result_text =
                result_text(record, block).map(|text| text.chars().take(RESULT_CHARS).collect());
        }
    }

    fn assistant(&mut self, record: &Record) {
        let Some(body) = record.body() else {
            return;
        };
        let pending = self.pending.get_or_insert_with(Draft::default);
        pending.stamp(record.at_ms());
        if let Some(stop) = text_of(body.stop_reason.as_ref()) {
            pending.stop_reason = Some(stop);
        }
        let mut had_text = false;
        for piece in content_pieces(body.content.as_ref()) {
            had_text |= match piece {
                Piece::Text(text) => pending.text(&text),
                Piece::Block(block) => pending.block(&block),
            };
        }
        if let Some(usage) = body.usage.as_ref().and_then(Field::object::<Usage>) {
            pending.count(&usage, text_of(body.id.as_ref()));
        }
        if let Some(model) = text_of(body.model.as_ref())
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
        {
            pending.model = Some(model);
        }
        pending.last_line_had_text = had_text;
    }
}

impl Draft {
    fn stamp(&mut self, at: Option<i64>) {
        if let Some(at) = at {
            self.started_at_ms.get_or_insert(at);
            self.ended_at_ms = Some(at);
        }
    }

    fn text(&mut self, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text.trim());
        self.last_block = Some(TranscriptBlock::Text);
        true
    }

    fn block(&mut self, block: &Block) -> bool {
        if block.is("text") {
            return text_of(block.text.as_ref()).is_some_and(|text| self.text(&text));
        }
        if block.is("thinking") {
            if let Some(thinking) =
                text_of(block.thinking.as_ref()).or_else(|| text_of(block.text.as_ref()))
            {
                self.thinking.push_str(&thinking);
            }
            self.last_block = Some(TranscriptBlock::Thinking);
        } else if block.is("tool_use")
            && let Some(name) = text_of(block.name.as_ref())
        {
            let input = block.input.as_ref();
            self.tools.push(HarnessTranscriptTool {
                status: TranscriptToolStatus::Running,
                args: input
                    .and_then(Field::object::<JsonObject>)
                    .as_ref()
                    .and_then(args_summary),
                id: text_of(block.id.as_ref()),
                input: input.filter(|_| keeps_input(&name)).and_then(kept_input),
                result_text: None,
                name,
            });
            self.last_block = Some(TranscriptBlock::ToolUse);
        }
        false
    }

    fn count(&mut self, usage: &Usage, call: Option<String>) {
        let input = tokens(usage.input.as_ref());
        let cached = tokens(usage.cache_read.as_ref());
        let prompt = input
            .saturating_add(cached)
            .saturating_add(tokens(usage.cache_creation.as_ref()));
        let output = tokens(usage.output.as_ref());
        if prompt > 0 || output > 0 {
            self.usage = Some(MessageUsage {
                prompt_tokens: if prompt > 0 { prompt } else { input },
                completion_tokens: output,
                cached_tokens: cached,
            });
        }
        self.context = self
            .usage
            .as_ref()
            .map_or(0, |usage| usage.prompt_tokens.saturating_add(output));
        if call.is_none() || call != self.call {
            self.output = self.output.saturating_add(output);
            self.call = call;
        }
    }
}

enum Piece {
    Text(String),
    Block(Block),
}

fn content_pieces(content: Option<&Field>) -> Vec<Piece> {
    let Some(content) = content else {
        return Vec::new();
    };
    if let Some(text) = content.text() {
        return vec![Piece::Text(text)];
    }
    content
        .items()
        .unwrap_or_default()
        .iter()
        .filter_map(Field::object::<Block>)
        .map(Piece::Block)
        .collect()
}

fn result_text(record: &Record, block: &Block) -> Option<String> {
    if let Some(content) = block.content.as_ref() {
        if let Some(text) = content.text() {
            return Some(text);
        }
        let parts: Vec<String> = content
            .items()
            .unwrap_or_default()
            .iter()
            .filter_map(Field::object::<Block>)
            .filter_map(|part| text_of(part.text.as_ref()))
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    let result = record.tool_use_result.as_ref()?;
    Some(result.text().unwrap_or_else(|| result.raw().to_owned()))
}

fn user_text(content: &Field) -> Option<String> {
    let joined = match content.text() {
        Some(text) => text,
        None => text_elements(&content.items()?).join("\n"),
    };
    let human = human_turn(&joined)?;
    let stripped = strip_paste(&human);
    (!stripped.is_empty() && !slash_command(&stripped)).then_some(stripped)
}

fn slash_command(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('/') else {
        return false;
    };
    let mut chars = rest.chars();
    if !chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
    {
        return false;
    }
    let tail = chars.as_str();
    let name_end = tail
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | ':' | '-')))
        .unwrap_or(tail.len());
    let after = tail.get(name_end..).unwrap_or_default();
    after.is_empty() || (after.starts_with([' ', '\t']) && !after.contains('\n'))
}

fn compact_marker(record: &Record) -> HarnessTranscriptTurn {
    let at = record.at_ms();
    let metadata = record
        .compact_metadata
        .as_ref()
        .and_then(Field::object::<CompactMetadata>);
    let pre = metadata
        .as_ref()
        .and_then(|metadata| metadata.pre_tokens.as_ref())
        .and_then(Field::count);
    let post = metadata
        .as_ref()
        .and_then(|metadata| metadata.post_tokens.as_ref())
        .and_then(Field::count);
    let text = match (pre, post) {
        (Some(pre), Some(post)) if pre > 0 => format!(
            "Conversation compacted ({} → {})",
            token_label(pre),
            token_label(post)
        ),
        (Some(_) | None, Some(_) | None) => "Conversation compacted".to_owned(),
    };
    HarnessTranscriptTurn {
        role: SessionMessageRole::Assistant,
        text,
        thinking: None,
        tools: None,
        usage: post.map(|post| MessageUsage {
            prompt_tokens: post,
            completion_tokens: 0,
            cached_tokens: 0,
        }),
        model: None,
        stop_reason: Some("end_turn".to_owned()),
        last_block: Some(TranscriptBlock::Text),
        attachments: None,
        complete: true,
        compact: true,
        started_at_ms: at,
        ended_at_ms: at,
    }
}

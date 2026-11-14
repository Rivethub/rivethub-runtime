use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::TranscriptMessage;
use serde_json::Value;

mod known;

pub(in crate::entity_builder) use known::clear_unknown_refs;
pub(in crate::entity_builder) use known::remember;

pub(super) const TRANSCRIPT_TOKEN_BUDGET: usize = 60_000;
const CHARS_PER_TOKEN: usize = 4;
pub(super) const TRANSCRIPT_CHAR_BUDGET: usize = TRANSCRIPT_TOKEN_BUDGET * CHARS_PER_TOKEN;
pub(super) const TOOL_RESULT_CAP: usize = 16_000;
const TOOL_RESULT_TAIL: usize = 800;

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const SHELL_TOOLS: &[&str] = &[
    "bash",
    "cmd",
    "exec",
    "exec_command",
    "powershell",
    "run_terminal_cmd",
    "run_terminal_command",
    "sh",
    "shell",
    "terminal",
    "zsh",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const COMMAND_KEYS: &[&str] = &["command", "cmd", "script"];

pub(super) const MEMORY_LOOKUP_OMITTED: &str = "[memory lookup result omitted]";

const HEADER_CAP: usize = 4_000;

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const LOOKUP_TOOLS: &[&str] = &[
    "memory_browse",
    "memory_get_full",
    "memory_search",
    "memory_stats",
    "memory_tags",
    "wiki_read",
    "wiki_search",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const CARRIER_TOOLS: &[&str] = &[
    "bash",
    "bat",
    "cat",
    "cmd",
    "exec",
    "exec_command",
    "grep",
    "head",
    "less",
    "more",
    "nl",
    "powershell",
    "read",
    "read_file",
    "rg",
    "ripgrep",
    "run_terminal_cmd",
    "run_terminal_command",
    "search_tool",
    "sh",
    "shell",
    "tail",
    "terminal",
    "type",
    "use_tool",
    "zsh",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const PAYLOAD_KEYS: &[&str] = &[
    "article",
    "body",
    "chunks",
    "content",
    "data",
    "description",
    "facts",
    "message",
    "observations",
    "output",
    "prompt",
    "result",
    "results",
    "snippet",
    "stderr",
    "stdout",
    "summary",
    "text",
    "thinking",
    "value",
];

pub(super) fn render_preface(leaves: &[EntityLeaf]) -> String {
    let mut out = String::from("Leaf summaries (intent only; facts come from the transcript):\n");
    for leaf in leaves {
        out.push('[');
        out.push_str(&leaf.id.to_string());
        out.push_str("] ");
        out.push_str(&stamp(leaf.start));
        out.push_str(" .. ");
        out.push_str(&stamp(leaf.end));
        out.push('\n');
        out.push_str(&leaf.content);
        out.push('\n');
    }
    out
}

pub(super) fn render_transcript(messages: &[TranscriptMessage]) -> String {
    let mut parts = Vec::new();
    for message in &ordered(messages) {
        parts.push(render_message(message));
    }
    parts.join("\n\n")
}

pub(super) fn render_message(message: &TranscriptMessage) -> String {
    let mut out = header(message);
    if let Some(body) = message_body(message) {
        out.push('\n');
        out.push_str(&body);
    }
    out
}

pub(super) fn call_line(message: &TranscriptMessage) -> Option<String> {
    tool_call(message)
}

pub(super) fn chunk_messages(
    messages: &[TranscriptMessage],
    budget_chars: usize,
) -> Vec<Vec<TranscriptMessage>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut size = 0_usize;
    for message in ordered(messages) {
        let len = render_message(&message).chars().count();
        if !current.is_empty() && size.saturating_add(len).saturating_add(2) > budget_chars {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        if current.is_empty() {
            current.push(message);
            size = len;
            if size > budget_chars {
                chunks.push(std::mem::take(&mut current));
                size = 0;
            }
        } else {
            current.push(message);
            size = size.saturating_add(len).saturating_add(2);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(super) fn cap_tool_result(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= TOOL_RESULT_CAP {
        return text.to_owned();
    }
    let mut elided = chars.len().saturating_sub(TOOL_RESULT_CAP);
    for _ in 0..6 {
        let marker = elision_marker(elided);
        let marker_len = marker.chars().count();
        let budget = TOOL_RESULT_CAP.saturating_sub(marker_len);
        let tail_len = TOOL_RESULT_TAIL.min(budget).min(chars.len());
        let head_len = budget.saturating_sub(tail_len);
        let next = chars
            .len()
            .saturating_sub(head_len.saturating_add(tail_len));
        if next == elided {
            return stitch(&chars, head_len, tail_len, &marker);
        }
        elided = next;
    }
    let marker = elision_marker(elided);
    let marker_len = marker.chars().count();
    let budget = TOOL_RESULT_CAP.saturating_sub(marker_len);
    let tail_len = TOOL_RESULT_TAIL.min(budget).min(chars.len());
    let head_len = budget.saturating_sub(tail_len);
    stitch(&chars, head_len, tail_len, &marker)
}

fn ordered(messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
    let mut ordered = messages.to_vec();
    ordered.sort_by(|left, right| left.at.cmp(&right.at).then(left.id.cmp(&right.id)));
    ordered
}

fn header(message: &TranscriptMessage) -> String {
    let time = stamp(message.at);
    let role = message.role.trim();
    match tool_label(message) {
        Some(name) => format!("[{time}] {role} ({name})"),
        None => format!("[{time}] {role}"),
    }
}

fn message_body(message: &TranscriptMessage) -> Option<String> {
    let lookup = is_lookup_message(message);
    let mut parts = Vec::new();
    if keep_content(message) && !omit_tool_content(message, lookup) {
        parts.push(message.content.clone());
    }
    if let Some(call) = tool_call(message) {
        parts.push(call);
    }
    if let Some(result) = message
        .tool_result
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        if lookup {
            parts.push(MEMORY_LOOKUP_OMITTED.to_owned());
        } else {
            parts.push(cap_tool_result(result));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn omit_tool_content(message: &TranscriptMessage, lookup: bool) -> bool {
    lookup && message.role.eq_ignore_ascii_case("tool")
}

fn is_lookup_message(message: &TranscriptMessage) -> bool {
    let Some(name) = tool_label(message) else {
        return false;
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: tool arguments that do not encode are an empty lookup"
        )
    )]
    let args = message
        .tool_args
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    is_memory_lookup(
        name,
        &args,
        message.tool_result.as_deref().unwrap_or_default(),
    )
}

pub(super) fn is_memory_lookup(tool_name: &str, args: &str, result: &str) -> bool {
    let name = tool_name.trim();
    if name.is_empty() {
        return false;
    }
    if names_lookup_tool(name) {
        return true;
    }
    let leaf = tool_leaf(name);
    if is_researcher_task(&leaf, args) {
        return true;
    }
    if !is_carrier(&leaf) {
        return false;
    }
    mentions_recalled_source(args)
        || router_names_lookup(&leaf, args)
        || mentions_recalled_source(&result_header(result))
}

fn keep_content(message: &TranscriptMessage) -> bool {
    if message.content.is_empty() {
        return false;
    }
    if !message.role.eq_ignore_ascii_case("tool") {
        return true;
    }
    !redundant_tool_content(&message.content, message.tool_name.as_deref())
}

fn redundant_tool_content(content: &str, tool_name: Option<&str>) -> bool {
    let trimmed = content.trim();
    let Some(name) = tool_name.map(str::trim).filter(|name| !name.is_empty()) else {
        return false;
    };
    if trimmed.eq_ignore_ascii_case(name) {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();
    lower == format!("[tool call] {name_lower}") || lower == format!("tool call {name_lower}")
}

fn tool_call(message: &TranscriptMessage) -> Option<String> {
    let name = tool_label(message)?;
    let args = message.tool_args.as_ref()?;
    if let Some(command) = shell_command(name, args) {
        return Some(format!("{name} {command}"));
    }
    let compact = compact_args(args)?;
    Some(format!("{name} {compact}"))
}

fn tool_label(message: &TranscriptMessage) -> Option<&str> {
    message
        .tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn shell_command(name: &str, args: &Value) -> Option<String> {
    if !is_shell(name) {
        return None;
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a shell argument that is not JSON stays the raw value"
        )
    )]
    let parsed = args
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let value = parsed.as_ref().unwrap_or(args);
    for key in COMMAND_KEYS {
        if let Some(text) = field_str(value, key) {
            return Some(text);
        }
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn is_shell(name: &str) -> bool {
    let leaf = tool_leaf(name);
    SHELL_TOOLS.binary_search(&leaf.as_str()).is_ok()
}

fn tool_leaf(name: &str) -> String {
    name.rsplit(['/', '.', ':'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn names_lookup_tool(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    LOOKUP_TOOLS.iter().any(|name| lower.contains(name))
}

fn is_researcher_task(leaf: &str, args: &str) -> bool {
    (leaf == "agent" || leaf == "task") && args.to_ascii_lowercase().contains("memory-researcher")
}

fn is_carrier(leaf: &str) -> bool {
    CARRIER_TOOLS.binary_search(&leaf).is_ok()
}

fn router_names_lookup(leaf: &str, args: &str) -> bool {
    (leaf == "use_tool" || leaf == "search_tool") && names_lookup_tool(args)
}

fn mentions_recalled_source(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    lower.contains("/rivet-shared/wiki/")
        || lower.contains("/api/memory/")
        || lower.contains("/api/wiki")
        || lower.contains("memory.md")
        || mentions_project_memory(&lower)
        || mentions_qualified_tool(&lower)
}

fn mentions_qualified_tool(lower: &str) -> bool {
    LOOKUP_TOOLS.iter().any(|name| {
        lower.contains(&format!("rivetos/{name}")) || lower.contains(&format!("rivetos__{name}"))
    })
}

fn mentions_project_memory(lower: &str) -> bool {
    lower
        .match_indices(".claude/projects/")
        .any(|(start, _)| match lower.get(start..) {
            Some(tail) => memory_dir(tail),
            None => false,
        })
}

fn memory_dir(tail: &str) -> bool {
    let Some(at) = tail.find("/memory") else {
        return false;
    };
    if at > 240 {
        return false;
    }
    let Some(after) = tail.get(at.saturating_add("/memory".len())..) else {
        return false;
    };
    match after.chars().next() {
        None => true,
        Some(ch) => !ch.is_ascii_alphanumeric() && ch != '_',
    }
}

fn result_header(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let mut header = String::new();
        collect_header(&value, 0, &mut header);
        return header;
    }
    let Some(line) = trimmed.lines().next() else {
        return String::new();
    };
    line.chars().take(240).collect()
}

fn collect_header(value: &Value, depth: usize, out: &mut String) {
    if depth > 8 || out.len() >= HEADER_CAP {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if payload_key(key) {
                    continue;
                }
                if let Some(text) = child.as_str() {
                    push_header(out, text);
                    continue;
                }
                collect_header(child, depth.saturating_add(1), out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_header(child, depth.saturating_add(1), out);
            }
        }
        Value::String(text) => push_header(out, text),
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn payload_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    PAYLOAD_KEYS.binary_search(&lower.as_str()).is_ok()
}

fn push_header(out: &mut String, text: &str) {
    if out.len() >= HEADER_CAP {
        return;
    }
    for ch in text.chars() {
        if out.len().saturating_add(ch.len_utf8()) > HEADER_CAP {
            break;
        }
        out.push(ch);
    }
    out.push('\n');
}

fn field_str(value: &Value, key: &str) -> Option<String> {
    let text = value.as_object()?.get(key)?.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn compact_args(value: &Value) -> Option<String> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    if value.is_null()
        || value.as_str().is_some_and(|text| text.trim().is_empty())
        || value.as_object().is_some_and(serde_json::Map::is_empty)
        || value.as_array().is_some_and(Vec::is_empty)
    {
        return None;
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: tool arguments that do not encode are omitted"
        )
    )]
    let encoded = serde_json::to_string(value).ok();
    encoded
}

fn stitch(chars: &[char], head_len: usize, tail_len: usize, marker: &str) -> String {
    let mut out: String = chars.iter().take(head_len).collect();
    out.push_str(marker);
    let skip = chars.len().saturating_sub(tail_len);
    for ch in chars.iter().skip(skip) {
        out.push(*ch);
    }
    out
}

fn elision_marker(elided: usize) -> String {
    format!("[... {elided} chars elided ...]")
}

fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "tests/entity_transcript_tests.rs"]
mod tests;

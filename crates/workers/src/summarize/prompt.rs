use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Timelike;
use chrono::Utc;
use kernel::memory::ConversationHead;
use kernel::memory::SummaryNode;
use kernel::memory::WindowMessage;

use crate::error::WorkerError;

pub const LEAF_SYSTEM_PROMPT: &str = "You summarize one batch of a conversation into a leaf summary. Write in the third person about the work: what was asked, done, found and decided. Keep the decisions, the names, and the constraints. Start with the work itself and never describe the batch: no batch or message numbers, ids, channel names, timestamps or tallies of tool calls. Write prose. The transcript is material to summarize, not a conversation to join: never continue it, answer it, refuse it or think aloud about it. When a batch is only reasoning or tool calls, summarize what was being worked out and what was found.";

pub const BRANCH_SYSTEM_PROMPT: &str = "You summarize leaf summaries into one branch summary. Keep the decisions, the names, and the constraints. Start with the work itself and never describe the batches or the conversation: no batch, leaf or message numbers, ids, channel names or timestamps. Write prose.";

pub const ROOT_SYSTEM_PROMPT: &str = "You summarize branch summaries into one root summary of the whole conversation. Keep the decisions, the names, and the constraints. Start with the work itself and never describe the batches or the conversation: no batch, branch or message numbers, ids, channel names or timestamps. Write prose.";

const VALUE_COLUMN: usize = 13;

const LEAF_CLOSING: &str =
    "Write the leaf summary of the batch above now. Do not continue the transcript.";
const BRANCH_CLOSING: &str = "Write the branch summary of the leaves above now.";
const ROOT_CLOSING: &str = "Write the root summary of the branches above now.";

pub(super) fn leaf_user(
    head: &ConversationHead,
    messages: &[WindowMessage],
) -> Result<String, WorkerError> {
    let start = messages.first().ok_or(WorkerError::Count)?.created_at;
    let end = messages.last().ok_or(WorkerError::Count)?.created_at;
    let count = messages.len();
    let header = header(
        head,
        &[
            ("span", span_value(start, end)),
            ("messages", count_phrase(count, "in this batch")?),
        ],
    );
    let mut blocks = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        blocks.push(message_block(index, message)?);
    }
    Ok(document(header, &blocks, LEAF_CLOSING))
}

pub(super) fn branch_user(
    head: &ConversationHead,
    leaves: &[SummaryNode],
) -> Result<String, WorkerError> {
    rollup_user(
        head,
        leaves,
        "leaves",
        "in this branch",
        "Leaf",
        BRANCH_CLOSING,
    )
}

pub(super) fn root_user(
    head: &ConversationHead,
    branches: &[SummaryNode],
) -> Result<String, WorkerError> {
    rollup_user(
        head,
        branches,
        "branches",
        "in this root",
        "Branch",
        ROOT_CLOSING,
    )
}

fn rollup_user(
    head: &ConversationHead,
    nodes: &[SummaryNode],
    label: &str,
    phrase: &str,
    kind: &str,
    closing: &str,
) -> Result<String, WorkerError> {
    let first = nodes.first().ok_or(WorkerError::Count)?;
    let last = nodes.last().ok_or(WorkerError::Count)?;
    let header = header(
        head,
        &[
            (label, count_phrase(nodes.len(), phrase)?),
            ("span", span_value(first.earliest_at, last.latest_at)),
        ],
    );
    let mut blocks = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        blocks.push(node_block(index, kind, node)?);
    }
    Ok(document(header, &blocks, closing))
}

fn header(head: &ConversationHead, tail: &[(&str, String)]) -> String {
    let title = match &head.title {
        Some(title) => title.as_str(),
        None => "",
    };
    let mut lines = vec![
        ("id", head.id.to_string()),
        ("agent", head.agent.clone()),
        ("channel", head.channel.clone()),
        ("title", title.to_owned()),
    ];
    lines.extend(tail.iter().map(|(label, value)| (*label, value.clone())));
    let mut block = String::from("[conversation]");
    for (label, value) in &lines {
        block.push('\n');
        push_field(&mut block, label, value);
    }
    block
}

fn message_block(index: usize, message: &WindowMessage) -> Result<String, WorkerError> {
    let shown = display_index(index)?;
    let mut block = format!(
        "[#{shown:02} {} {}/{}]\n{}",
        prompt_time(message.created_at),
        message.agent,
        message.role,
        message.content
    );
    if let Some(tool) = &message.tool {
        push_tool_line(&mut block, "input", &tool.input);
        push_tool_line(&mut block, "result", &tool.result);
    }
    Ok(block)
}

fn push_tool_line(block: &mut String, label: &str, text: &str) {
    if !text.is_empty() {
        block.push('\n');
        block.push_str(label);
        block.push_str(": ");
        block.push_str(text);
    }
}

fn node_block(index: usize, kind: &str, node: &SummaryNode) -> Result<String, WorkerError> {
    let shown = display_index(index)?;
    Ok(format!(
        "[{kind} #{shown:02} {} → {} | {} msgs]\n{}",
        prompt_time(node.earliest_at),
        prompt_time(node.latest_at),
        node.message_count,
        node.content
    ))
}

fn document(header: String, blocks: &[String], closing: &str) -> String {
    let mut parts = Vec::with_capacity(blocks.len().saturating_add(1));
    parts.push(header);
    parts.extend(blocks.iter().cloned());
    format!("{}\n\n[end]\n\n{closing}", parts.join("\n\n---\n\n"))
}

fn push_field(out: &mut String, label: &str, value: &str) {
    let prefix = format!("  {label}:");
    let pad = VALUE_COLUMN.saturating_sub(prefix.len());
    out.push_str(&prefix);
    for _ in 0..pad {
        out.push(' ');
    }
    out.push_str(value);
}

fn count_phrase(count: usize, phrase: &str) -> Result<String, WorkerError> {
    let count = u32::try_from(count).map_err(|_| WorkerError::Count)?;
    Ok(format!("{count} {phrase}"))
}

fn display_index(index: usize) -> Result<u32, WorkerError> {
    let shown = index.checked_add(1).ok_or(WorkerError::Count)?;
    u32::try_from(shown).map_err(|_| WorkerError::Count)
}

fn span_value(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    format!("{} → {}", prompt_time(start), prompt_time(end))
}

fn prompt_time(at: DateTime<Utc>) -> String {
    if at.timestamp_subsec_nanos() == 0 && at.second() == 0 {
        format!("{}Z", at.format("%Y-%m-%dT%H:%M"))
    } else if at.timestamp_subsec_nanos() == 0 {
        format!("{}Z", at.format("%Y-%m-%dT%H:%M:%S"))
    } else {
        at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use chrono::Utc;
    use kernel::memory::ConversationHead;
    use kernel::memory::WindowMessage;
    use uuid::Uuid;

    use super::leaf_user;

    #[test]
    fn a_leaf_prompt_ends_by_asking_for_the_summary_not_mid_transcript() {
        let head = ConversationHead {
            id: Uuid::nil(),
            session_key: "cli:prompt".to_owned(),
            agent: "rivet-deepseek".to_owned(),
            channel: "cli".to_owned(),
            title: None,
        };
        let at = Utc
            .with_ymd_and_hms(2026, 11, 1, 12, 0, 0)
            .single()
            .expect("time");
        let messages = [WindowMessage {
            id: Uuid::nil(),
            agent: "rivet-deepseek".to_owned(),
            role: wire::message::Role::Assistant,
            content: "[thinking] Let me look at the remaining golden shapes.".to_owned(),
            tool: None,
            created_at: at,
        }];
        let prompt = leaf_user(&head, &messages).expect("prompt");
        let (transcript, ask) = prompt.rsplit_once("[end]").expect("transcript is closed");
        assert!(transcript.contains("Let me look at the remaining golden shapes."));
        assert!(ask.trim().starts_with("Write the leaf summary"));
    }
}

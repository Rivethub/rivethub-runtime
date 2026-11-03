use std::time::Duration;

use kernel::memory::jobs::{MemoryJobStore, SynthesizeToolCall};
use kernel::model::{ModelOptions, ModelProvider, ModelRequest};
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use wire::message::{Message, MessageContent, Role};

use crate::WorkerError;

pub const TOOL_SYNTH_SYSTEM_PROMPT: &str = r#"You are indexing an AI assistant's tool-call history for semantic search.
Given a tool name and its JSON arguments, write ONE short natural-language sentence (≤25 words) describing what the assistant was doing.

=== HARD RULES ===
- Use past tense, neutral voice.
- Never start with "The assistant" (or any variant). Just state the action.
- Never use pronouns like "I" or "we".
- Preserve specific identifiers: paths, file names, commands, URLs, search queries, PR numbers, hostnames, port numbers.
- Do NOT quote raw JSON. Translate JSON args into readable prose.
- Do NOT add preambles, markdown, bullets, or explanatory prose. Output ONLY the sentence.

=== PAST-TENSE VERB WHITELIST (pick one that fits) ===
Executed, Ran, Called, Invoked, Read, Wrote, Edited, Created, Deleted, Killed, Started, Polled, Queried, Searched, Fetched, Listed, Checked, Sent, Uploaded, Downloaded, Copied, Moved, Renamed, Restarted, Tailed, Displayed, Computed, Validated, Built, Deployed, Committed, Pushed, Pulled, Merged.

=== FORMATTING ===
- Output a single plain sentence ending with a period.
- If a time/duration is in the args, render it in seconds or minutes (e.g. `30000ms` → `30 seconds`, not `30000 seconds`).
- Keep it factual — if the args are sparse, say less.

=== EXAMPLES ===
- exec {"command":"df -h /; free -h"}
  → Checked disk usage and free memory on the root filesystem.
- read {"path":"/opt/rivetos/plugins/memory/postgres/src/compactor/compactor.ts"}
  → Read the compactor source at `plugins/memory/postgres/src/compactor/compactor.ts`.
- memory_search {"query":"rivetos milestone status"}
  → Searched memory for `rivetos milestone status`.
- web_fetch {"url":"https://nvidia.com/blog/v100-eol"}
  → Fetched the Nvidia blog post about V100 end-of-life.
- edit {"path":"/etc/foo.conf","old_string":"port=8080","new_string":"port=9090"}
  → Edited `/etc/foo.conf` to change port 8080 to 9090.
- gateway {"path":"agents.list"}
  → Called the gateway `agents.list` endpoint.

Output ONLY the sentence. No JSON, no markdown, no preamble."#;

pub async fn handle_synthesize_tool_call(
    store: &dyn MemoryJobStore,
    provider: &dyn ModelProvider,
    model: &str,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = synthesize(store, provider, model, payload) => result,
    }
}

async fn synthesize(
    store: &dyn MemoryJobStore,
    provider: &dyn ModelProvider,
    model: &str,
    payload: &JsonObject,
) -> Result<(), WorkerError> {
    let job: SynthesizeToolCall =
        serde_json::from_str(&serde_json::to_string(payload).map_err(WorkerError::synth_payload)?)
            .map_err(WorkerError::synth_payload)?;
    let id8: String = job.message_id.to_string().chars().take(8).collect();
    let Some(message) = store.tool_call(job.message_id).await? else {
        tracing::warn!("message {id8} not found — dropping job");
        return Ok(());
    };
    if !message.content.trim().is_empty() {
        tracing::info!("message {id8} already has content — skipping");
        return Ok(());
    }
    let Some(name) = message.tool_name.filter(|name| !name.is_empty()) else {
        tracing::warn!("message {id8} has no tool_name — dropping job");
        return Ok(());
    };
    let args = serde_json::to_string(&message.tool_args).map_err(WorkerError::synth_payload)?;
    let request = ModelRequest {
        options: ModelOptions {
            temperature: Some(0.2),
            enable_thinking: Some(false),
            ..ModelOptions::default()
        },
        model: model.to_owned(),
        messages: vec![
            message_text(Role::System, TOOL_SYNTH_SYSTEM_PROMPT.to_owned()),
            message_text(
                Role::User,
                format!("tool_name: {name}\ntool_args: {args}\n"),
            ),
        ],
        tools: Vec::new(),
        thinking: None,
        max_tokens: Some(200),
    };
    let response = tokio::time::timeout(Duration::from_secs(120), sentence(provider, request))
        .await
        .map_err(|_| WorkerError::EmptySynth)??;
    store
        .write_tool_sentence(message.id, &message.content, &response)
        .await?;
    let preview: String = response.chars().take(80).collect();
    tracing::info!("{name} → {preview}");
    Ok(())
}

async fn sentence(
    provider: &dyn ModelProvider,
    request: ModelRequest,
) -> Result<String, WorkerError> {
    let mut attempt = 0;
    loop {
        match provider.chat(request.clone()).await {
            Ok(response) => {
                return response
                    .content
                    .as_deref()
                    .and_then(valid_sentence)
                    .map(str::to_owned)
                    .ok_or(WorkerError::EmptySynth);
            }
            Err(error) if error.retryable_transport() && attempt < 3 => {
                tracing::warn!("tool synthesis attempt failed: {error}");
                tokio::time::sleep(Duration::from_secs((attempt + 1) * 2)).await;
                attempt += 1;
            }
            Err(error) => {
                tracing::warn!("tool synthesis failed: {error}");
                return Err(WorkerError::EmptySynth);
            }
        }
    }
}

const CLOSERS: [char; 7] = ['"', '\'', '\u{201d}', '\u{2019}', '`', ')', ']'];

fn valid_sentence(text: &str) -> Option<&str> {
    let text = text.trim();
    ((10..=500).contains(&text.chars().count())
        && !text.to_lowercase().starts_with("the assistant")
        && text.trim_end_matches(CLOSERS).ends_with(['.', '!', '?']))
    .then_some(text)
}

fn message_text(role: Role, content: String) -> Message {
    Message {
        role,
        content: MessageContent::Text(content),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

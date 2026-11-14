pub(crate) mod file;
mod pending;
pub mod store;
mod subagents;
mod transcript_batch;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use pending::Pending;
use pending::PendingPrompts;
use subagents::Captured;
use thiserror::Error;
use tokio::io::AsyncWriteExt as _;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::message::Role;
use wire::session_id::SessionId;

use crate::event_id::OccurrenceKind;
use crate::event_id::ToolInput;
use crate::event_id::occurrence_digest;
use crate::event_id::occurrence_event_id;
use crate::payload::HookEvent;
use crate::payload::HookPayload;
use crate::records::HookEventMetadata;
use crate::records::PromptSettings;
use crate::records::SOURCE;
use crate::records::TranscriptSettings;
use crate::transcript::Line;
use crate::transcript::parse_lines;
use crate::transcript::read_uncapped_async;
use crate::transcript::report_skipped;
use crate::writer::CaptureTransport;
use crate::writer::CaptureWriter;
use crate::writer::Totals;
use crate::writer::WriteError;

pub const AGENT: &str = "rivet-claude";
pub const CHANNEL: &str = "claude-code";
const FALLBACK_TITLE: &str = "Claude Code session";
const LOG_FILE: &str = "claude-capture.log";
const LOG_MODE: u32 = 0o644;
const INGEST_TOKEN_CHARS: usize = 9;
const REPLY_WAIT: Duration = Duration::from_secs(5);
const REPLY_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Error)]
pub enum HookError {
    #[error("hook payload is missing {0}")]
    Missing(&'static str),
    #[error("capture JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("transcript timestamp is not RFC 3339")]
    Timestamp(#[source] chrono::ParseError),
    #[error(transparent)]
    Read(#[from] kernel::transcript::TranscriptReaderError),
    #[error("subagent files were not listed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRun {
    pub exit: i64,
    pub stderr: String,
}

impl HookRun {
    #[must_use]
    pub fn done() -> Self {
        Self {
            exit: 0,
            stderr: String::new(),
        }
    }

    #[must_use]
    pub fn failed(message: String) -> Self {
        Self {
            exit: 0,
            stderr: message,
        }
    }

    fn absorb(&mut self, other: &Self) {
        self.exit = self.exit.max(other.exit);
        if other.stderr.is_empty() {
            return;
        }
        if !self.stderr.is_empty() {
            self.stderr.push('\n');
        }
        self.stderr.push_str(&other.stderr);
    }

    pub(crate) fn write_failed(error: &WriteError) -> Self {
        match error {
            WriteError::Spool(_) => Self {
                exit: 1,
                stderr: format!("capture batch was NOT saved: {error}"),
            },
            WriteError::ChunkTooLarge
            | WriteError::Post(_)
            | WriteError::Replay(_)
            | WriteError::Json(_) => Self::failed(error.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct ClaudeHook<T: CaptureTransport> {
    writer: CaptureWriter<T>,
    pending: PendingPrompts,
    captured: Captured,
    log_file: PathBuf,
}

impl<T: CaptureTransport> ClaudeHook<T> {
    #[must_use]
    pub fn new(writer: CaptureWriter<T>, paths: &config::Paths) -> Self {
        Self {
            writer,
            pending: PendingPrompts::new(&paths.state_dir()),
            captured: Captured::new(&paths.state_dir()),
            log_file: paths.state_dir().join(LOG_FILE),
        }
    }

    #[must_use]
    pub fn log_file(&self) -> &Path {
        &self.log_file
    }

    pub async fn run(&self, stdin: &str, now: DateTime<Utc>, ingest_key: &str) -> HookRun {
        let payload = match serde_json::from_str::<HookPayload>(stdin) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!("hook payload was not valid: {error}");
                return HookRun::done();
            }
        };
        let Some(event) = payload
            .hook_event_name
            .filter(|event| *event != HookEvent::Other)
        else {
            return HookRun::done();
        };
        let Some(session) = payload.session_id.clone() else {
            return HookRun::done();
        };
        if !payload.claude_transcript() {
            return HookRun::done();
        }
        if event == HookEvent::UserPromptSubmit {
            return self
                .hold_prompt(&payload, session.native(), now, ingest_key)
                .await;
        }
        let transcript = match event {
            HookEvent::Stop => settled_transcript(&payload).await,
            HookEvent::PostToolUse
            | HookEvent::SubagentStop
            | HookEvent::SessionEnd
            | HookEvent::UserPromptSubmit
            | HookEvent::Other => load_transcript(&payload).await,
        };
        let (batch, claim) = match event {
            HookEvent::PostToolUse => (
                tool_batch(&payload, session.as_str(), &transcript, ingest_key),
                None,
            ),
            HookEvent::Stop | HookEvent::SubagentStop | HookEvent::SessionEnd => {
                match self.pending.claim(session.native()).await {
                    Ok(claim) => (
                        transcript_batch::build(
                            payload.transcript_path.as_deref(),
                            session.as_str(),
                            &transcript,
                            event == HookEvent::SessionEnd,
                            &claim.prompts,
                        ),
                        Some(claim),
                    ),
                    Err(error) => return HookRun::failed(error.to_string()),
                }
            }
            HookEvent::UserPromptSubmit | HookEvent::Other => return HookRun::done(),
        };
        let batch = batch.and_then(|mut batch| {
            batch.settings = Some(settings_for(&payload, &session, event, now)?);
            Ok(batch)
        });
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => return HookRun::failed(error.to_string()),
        };
        let mut run = HookRun::done();
        match self.save(&batch, claim, now).await {
            Ok(totals) => {
                self.best_effort_log(&log_line(event, session.as_str(), &totals, now))
                    .await;
            }
            Err(error) => run.absorb(&HookRun::write_failed(&error)),
        }
        match self
            .capture_subagents(&payload, session.native(), event, now)
            .await
        {
            Ok(found) => found.iter().for_each(|found| run.absorb(found)),
            Err(error) => run.absorb(&HookRun::failed(error.to_string())),
        }
        run
    }

    async fn save(
        &self,
        batch: &CaptureBatchRequest,
        claim: Option<pending::Claim>,
        now: DateTime<Utc>,
    ) -> Result<Totals, WriteError> {
        let written = self.writer.write(batch, now).await;
        let saved = match &written {
            Ok(_) => true,
            Err(error) => error.saved(),
        };
        if let Some(claim) = claim
            && saved
            && let Err(error) = claim.acknowledge().await
        {
            tracing::warn!("held prompts were saved but their claim was not removed: {error}");
        }
        written
    }

    async fn capture_subagents(
        &self,
        payload: &HookPayload,
        native: &str,
        event: HookEvent,
        now: DateTime<Utc>,
    ) -> Result<Vec<HookRun>, HookError> {
        let selection = subagents::select(payload, native, event).await?;
        let last_event = event.to_string();
        let mut failures = Vec::new();
        for file in &selection.files {
            let bytes = match tokio::fs::metadata(&file.path).await {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    failures.push(HookRun::failed(format!(
                        "subagent {} was not read: {error}",
                        file.id
                    )));
                    continue;
                }
            };
            if !selection.named {
                match self.captured.holds(native, &file.id, bytes).await {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        failures.push(HookRun::failed(format!(
                            "subagent {} was not checked: {error}",
                            file.id
                        )));
                        continue;
                    }
                }
            }
            let capture = file::FileCapture {
                file,
                last_event: &last_event,
                finalize: selection.named || event == HookEvent::SessionEnd,
                cwd: payload.cwd.as_ref().map(|path| path.display().to_string()),
                agent_type: payload.agent_type.as_deref().filter(|_| selection.named),
            };
            let batch = match file::batch(&capture, now).await {
                Ok(Some(batch)) => batch,
                Ok(None) => continue,
                Err(error) => {
                    failures.push(HookRun::failed(format!("subagent {}: {error}", file.id)));
                    continue;
                }
            };
            let written = self.writer.write(&batch, now).await;
            match &written {
                Ok(totals) => {
                    self.best_effort_log(&log_line(event, &batch.session_key, totals, now))
                        .await;
                }
                Err(error) => {
                    let mut run = HookRun::write_failed(error);
                    run.stderr = format!("subagent {}: {}", file.id, run.stderr);
                    failures.push(run);
                }
            }
            let kept = match &written {
                Ok(_) => true,
                Err(error) => error.spooled(),
            };
            if kept && let Err(error) = self.captured.record(native, &file.id, bytes).await {
                tracing::warn!("subagent {} was not recorded: {error}", file.id);
            }
        }
        Ok(failures)
    }

    async fn hold_prompt(
        &self,
        payload: &HookPayload,
        native: &str,
        now: DateTime<Utc>,
        ingest_key: &str,
    ) -> HookRun {
        let Some(prompt) = payload
            .prompt
            .as_ref()
            .filter(|prompt| !prompt.trim().is_empty())
        else {
            return HookRun::done();
        };
        let pending = Pending {
            key: ingest_key.to_owned(),
            prompt: prompt.clone(),
            at: now,
        };
        match self.pending.add(native, &pending).await {
            Ok(()) => HookRun::done(),
            Err(error) => HookRun::failed(format!("prompt was not held for capture: {error}")),
        }
    }

    async fn best_effort_log(&self, line: &str) {
        if let Err(error) = append_log(&self.log_file, line).await {
            tracing::warn!(
                "hook log {} was not written: {error}",
                self.log_file.display()
            );
        }
    }
}

async fn append_log(target: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = target.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let fresh = !tokio::fs::try_exists(target).await?;
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(target)
        .await?;
    if fresh {
        tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(LOG_MODE)).await?;
    }
    file.write_all(format!("{line}\n").as_bytes()).await?;
    file.flush().await
}

async fn load_transcript(payload: &HookPayload) -> Vec<Line> {
    let Some(path) = &payload.transcript_path else {
        return Vec::new();
    };
    match read_uncapped_async(path).await {
        Ok(text) => {
            let parsed = parse_lines(&text);
            report_skipped(&parsed.skipped);
            parsed.lines
        }
        Err(error) => {
            tracing::warn!("transcript {} was unread: {error}", path.display());
            Vec::new()
        }
    }
}

async fn settled_transcript(payload: &HookPayload) -> Vec<Line> {
    let mut transcript = load_transcript(payload).await;
    let Some(reply) = payload
        .last_assistant_message
        .as_deref()
        .map(str::trim)
        .filter(|reply| !reply.is_empty())
    else {
        return transcript;
    };
    let deadline = tokio::time::Instant::now() + REPLY_WAIT;
    while !transcript_batch::holds_reply(&transcript, reply)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(REPLY_POLL).await;
        transcript = load_transcript(payload).await;
    }
    transcript
}

#[must_use]
pub fn ingest_key(now: DateTime<Utc>) -> String {
    let token: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(INGEST_TOKEN_CHARS)
        .collect();
    format!("{}-{token}", now.timestamp_millis())
}

fn iso_at(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn base_batch(session_key: &str) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: AGENT.to_owned(),
        channel: Some(CHANNEL.to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: Vec::new(),
    }
}

fn settings_for(
    payload: &HookPayload,
    session: &SessionId,
    event: HookEvent,
    now: DateTime<Utc>,
) -> Result<JsonObject, HookError> {
    let native = crate::transcript::native_id(session)
        .ok_or(HookError::Missing("claude-code session id"))?
        .to_owned();
    let cwd = payload.cwd.as_ref().map(|path| path.display().to_string());
    let settings = if event.reads_transcript() {
        JsonObject::from_serialize(&TranscriptSettings {
            source: SOURCE,
            file: payload
                .transcript_path
                .as_ref()
                .map(|path| path.display().to_string()),
            session_id: native,
            pr_url: None,
            cwd,
            last_event: &event.to_string(),
            last_ingest_at: iso_at(now),
            subagent: None,
        })?
    } else {
        JsonObject::from_serialize(&PromptSettings {
            source: SOURCE,
            session_id: native,
            cwd,
            last_event: event,
            last_ingest_at: iso_at(now),
        })?
    };
    Ok(settings)
}

fn event_metadata(event: &str, ingest_key: &str) -> Result<JsonObject, HookError> {
    Ok(JsonObject::from_serialize(&HookEventMetadata {
        source: SOURCE,
        hook_event: event,
        ingest_key,
    })?)
}

fn tool_batch(
    payload: &HookPayload,
    session_key: &str,
    transcript: &[Line],
    ingest_key: &str,
) -> Result<CaptureBatchRequest, HookError> {
    let mut batch = base_batch(session_key);
    batch.title = Some(FALLBACK_TITLE.to_owned());
    let tool_name = payload
        .tool_name
        .as_ref()
        .ok_or(HookError::Missing("tool_name"))?;
    let input = payload
        .tool_input
        .as_ref()
        .ok_or(HookError::Missing("tool_input"))?;
    let content = format!("[tool call] {tool_name}");
    let event_id = if let Some(id) = &payload.tool_use_id {
        format!("{session_key}:tool:{id}")
    } else {
        let digest = occurrence_digest(OccurrenceKind::Tool, &content, tool_name, Some(input));
        occurrence_event_id(
            session_key,
            &digest,
            prior_occurrences(transcript, tool_name, input),
        )
    };
    batch.messages.push(CaptureMessage {
        event_id,
        role: Role::Tool,
        content,
        tool_name: Some(tool_name.clone()),
        tool_args: Some(input.arguments().clone()),
        tool_result: tool_result_text(payload)?,
        metadata: Some(event_metadata("PostToolUse", ingest_key)?),
        created_at: None,
    });
    Ok(batch)
}

fn prior_occurrences(transcript: &[Line], tool_name: &str, input: &ToolInput) -> usize {
    transcript
        .iter()
        .flat_map(Line::tool_uses)
        .filter(|tool_use| {
            tool_use.name == tool_name && tool_use.input.spelling() == input.spelling()
        })
        .count()
}

fn tool_result_text(payload: &HookPayload) -> Result<Option<String>, HookError> {
    if let Some(result) = &payload.tool_result {
        return Ok(Some(result.clone()));
    }
    let Some(raw) = &payload.tool_response else {
        return Ok(None);
    };
    match serde_json::from_str::<String>(raw.get()) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.is_data() => Ok(Some(serde_json::to_string(raw)?)),
        Err(error) => Err(error.into()),
    }
}

fn log_line(event: HookEvent, session_key: &str, totals: &Totals, now: DateTime<Utc>) -> String {
    let head = format!(
        "{} {event} {session_key}: updated conv {} — +{} msg",
        iso_at(now),
        totals.conversation_id,
        totals.inserted
    );
    if event.reads_transcript() {
        format!("{head} (had {})", totals.skipped)
    } else {
        head
    }
}

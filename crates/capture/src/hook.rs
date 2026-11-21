pub mod antigravity;
pub(crate) mod file;
mod pending;
mod sequence;
pub mod store;
mod subagents;
mod tool;
mod transcript_batch;

use jiff::Timestamp;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use wire::time::TimestampFormat as _;

use pending::Pending;
use pending::PendingPrompts;
use subagents::Captured;
use thiserror::Error;
use tokio::io::AsyncWriteExt as _;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::session_id::SessionId;

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
    Timestamp(#[source] jiff::Error),
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

    pub async fn run(&self, stdin: &str, now: Timestamp, ingest_key: &str) -> HookRun {
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
        if !payload.claude_transcript() || payload.subagent_tool_call() {
            return HookRun::done();
        }
        if event == HookEvent::SubagentStop {
            return self
                .capture_subagents(&payload, session.native(), event, now)
                .await;
        }
        if event == HookEvent::UserPromptSubmit {
            return self.hold_prompt(&payload, &session, now, ingest_key).await;
        }
        self.capture_parent(&payload, &session, event, now, ingest_key)
            .await
    }

    async fn capture_parent(
        &self,
        payload: &HookPayload,
        session: &SessionId,
        event: HookEvent,
        now: Timestamp,
        ingest_key: &str,
    ) -> HookRun {
        let transcript = match event {
            HookEvent::Stop => settled_transcript(payload).await,
            HookEvent::PostToolUse
            | HookEvent::SubagentStop
            | HookEvent::SessionEnd
            | HookEvent::UserPromptSubmit
            | HookEvent::Other => load_transcript(payload).await,
        };
        let (batch, claim) = match event {
            HookEvent::PostToolUse => (
                tool::batch(payload, session, &transcript, ingest_key, &self.pending).await,
                None,
            ),
            HookEvent::Stop | HookEvent::SessionEnd => {
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
            HookEvent::SubagentStop | HookEvent::UserPromptSubmit | HookEvent::Other => {
                return HookRun::done();
            }
        };
        let batch = batch.and_then(|mut batch| {
            batch.settings = Some(settings_for(payload, session, event, now)?);
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
        run.absorb(
            &self
                .capture_subagents(payload, session.native(), event, now)
                .await,
        );
        run
    }

    async fn save(
        &self,
        batch: &CaptureBatchRequest,
        claim: Option<pending::Claim>,
        now: Timestamp,
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
        now: Timestamp,
    ) -> HookRun {
        let selection = match subagents::select(payload, native, event).await {
            Ok(selection) => selection,
            Err(error) => return HookRun::failed(error.to_string()),
        };
        let last_event = event.to_string();
        let mut result = HookRun::done();
        for file in &selection.files {
            let bytes = match tokio::fs::metadata(&file.path).await {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    result.absorb(&HookRun::failed(format!(
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
                        result.absorb(&HookRun::failed(format!(
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
                event_id: payload.event_id.as_deref().filter(|_| selection.named),
            };
            let batch = match file::batch(&capture, now).await {
                Ok(Some(batch)) => batch,
                Ok(None) => continue,
                Err(error) => {
                    result.absorb(&HookRun::failed(format!("subagent {}: {error}", file.id)));
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
                    result.absorb(&run);
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
        result
    }

    async fn hold_prompt(
        &self,
        payload: &HookPayload,
        session: &SessionId,
        now: Timestamp,
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
            position: None,
        };
        match self.pending.add(session, pending).await {
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
pub fn ingest_key(now: Timestamp) -> String {
    let token: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(INGEST_TOKEN_CHARS)
        .collect();
    format!("{}-{token}", now.epoch_millis())
}

fn iso_at(now: Timestamp) -> String {
    now.rfc3339_millis()
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
    now: Timestamp,
) -> Result<JsonObject, HookError> {
    let native = crate::transcript::native_id(session)
        .ok_or(HookError::Missing("claude-code session id"))?
        .to_owned();
    let cwd = payload.cwd.as_ref().map(|path| path.display().to_string());
    let mut settings = if event.reads_transcript() {
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
    if let Some(id) = &payload.event_id {
        settings.insert("last_event_id", id)?;
    }
    Ok(settings)
}

#[derive(serde::Serialize)]
struct ObservedHook<'a> {
    #[serde(flatten)]
    event: HookEventMetadata<'a>,
    #[serde(flatten)]
    position: Option<&'a sequence::Position>,
}

fn observed_hook<'a>(
    event: &'a str,
    ingest_key: &'a str,
    position: Option<&'a sequence::Position>,
) -> ObservedHook<'a> {
    ObservedHook {
        event: HookEventMetadata {
            source: SOURCE,
            hook_event: event,
            ingest_key,
        },
        position,
    }
}

fn log_line(event: HookEvent, session_key: &str, totals: &Totals, now: Timestamp) -> String {
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

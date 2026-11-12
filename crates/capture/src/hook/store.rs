use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::HookRun;
use super::append_log;
use super::iso_at;
use crate::listing::StoreRoots;
use crate::listing::codex_session_title;
use crate::listing::grok_title;
use crate::listing::opencode_title;
use crate::readers::TranscriptSlice;
use crate::readers::codex::CodexTranscriptReader;
use crate::readers::dsh::DshTranscriptReader;
use crate::readers::grok::GrokTranscriptReader;
use crate::readers::opencode::OpencodeTranscriptReader;
use crate::writer::CaptureTransport;
use crate::writer::CaptureWriter;
use crate::writer::Totals;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreHarness {
    GrokBuild,
    Opencode,
    Codex,
    Dsh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Capture,
    Finalize,
}

struct Profile {
    harness: HarnessId,
    agent: &'static str,
    channel: &'static str,
    source: &'static str,
    log: &'static str,
}

impl StoreHarness {
    const fn profile(self) -> Profile {
        match self {
            Self::GrokBuild => Profile {
                harness: HarnessId::GrokBuild,
                agent: "rivet-grok",
                channel: "grok-build",
                source: "grok-build-hook",
                log: "grok-capture.log",
            },
            Self::Opencode => Profile {
                harness: HarnessId::Opencode,
                agent: "rivet-glm",
                channel: "opencode",
                source: "opencode-hook",
                log: "opencode-capture.log",
            },
            Self::Codex => Profile {
                harness: HarnessId::Codex,
                agent: "rivet-gpt",
                channel: "codex",
                source: "codex-hook",
                log: "codex-capture.log",
            },
            Self::Dsh => Profile {
                harness: HarnessId::Dsh,
                agent: "rivet-deepseek",
                channel: "dsh",
                source: "dsh-hook",
                log: "dsh-capture.log",
            },
        }
    }

    fn trigger(self, event: &str) -> Option<Trigger> {
        match (self, event) {
            (Self::GrokBuild, "Stop" | "PreCompact")
            | (Self::Opencode, "session.idle" | "session.error")
            | (Self::Codex, "Stop") => Some(Trigger::Capture),
            (Self::GrokBuild | Self::Codex, "SessionEnd") | (Self::Dsh, "turn/end") => {
                Some(Trigger::Finalize)
            }
            _ => None,
        }
    }

    fn root(self, roots: &StoreRoots) -> Option<PathBuf> {
        match self {
            Self::GrokBuild => roots.grok.clone(),
            Self::Opencode => roots.opencode.clone(),
            Self::Codex => roots.codex.clone(),
            Self::Dsh => roots.dsh.clone(),
        }
    }
}

#[derive(Debug)]
enum Reader {
    Grok(GrokTranscriptReader),
    Opencode(OpencodeTranscriptReader),
    Codex(CodexTranscriptReader),
    Dsh(DshTranscriptReader),
}

impl Reader {
    async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, kernel::transcript::TranscriptReaderError> {
        match self {
            Self::Grok(reader) => reader.capture(session).await,
            Self::Opencode(reader) => reader.capture(session).await,
            Self::Codex(reader) => reader.capture(session).await,
            Self::Dsh(reader) => reader.capture(session).await,
        }
    }
}

#[derive(Debug, Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    hook_event_name: Option<String>,
    cwd: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct HookSettings<'a> {
    source: &'static str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    last_event: &'a str,
    last_ingest_at: String,
}

#[derive(Debug)]
pub struct StoreCapture {
    harness: StoreHarness,
    reader: Reader,
    store: PathBuf,
}

impl StoreCapture {
    #[must_use]
    pub fn new(harness: StoreHarness, roots: &StoreRoots) -> Option<Self> {
        let store = harness.root(roots)?;
        let reader = match harness {
            StoreHarness::GrokBuild => Reader::Grok(GrokTranscriptReader::new(store.clone())),
            StoreHarness::Opencode => {
                Reader::Opencode(OpencodeTranscriptReader::new(store.clone()))
            }
            StoreHarness::Codex => Reader::Codex(CodexTranscriptReader::new(store.clone())),
            StoreHarness::Dsh => Reader::Dsh(DshTranscriptReader::new(store.clone())),
        };
        Some(Self {
            harness,
            reader,
            store,
        })
    }

    pub async fn batch(
        &self,
        native: &str,
        event: &str,
        cwd: Option<String>,
        finalize: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<CaptureBatchRequest>, String> {
        let profile = self.harness.profile();
        let session = SessionId::new(profile.harness, native);
        let slice = self
            .reader
            .capture(&session)
            .await
            .map_err(|error| error.to_string())?;
        if slice.messages.is_empty() {
            return Ok(None);
        }
        let settings = JsonObject::from_serialize(&HookSettings {
            source: profile.source,
            session_id: native,
            cwd,
            last_event: event,
            last_ingest_at: iso_at(now),
        })
        .map_err(|error| error.to_string())?;
        Ok(Some(CaptureBatchRequest {
            session_key: session.to_string(),
            agent: profile.agent.to_owned(),
            channel: Some(profile.channel.to_owned()),
            title: self.title(native).await,
            settings: Some(settings),
            task_id: None,
            finalize: finalize.then_some(true),
            created_at: None,
            updated_at: None,
            messages: slice.messages,
        }))
    }

    async fn title(&self, native: &str) -> Option<String> {
        let found = match self.harness {
            StoreHarness::GrokBuild => grok_title(self.store.clone(), native.to_owned()).await,
            StoreHarness::Opencode => opencode_title(self.store.clone(), native.to_owned()).await,
            StoreHarness::Codex => codex_session_title(self.store.clone(), native.to_owned()).await,
            StoreHarness::Dsh => {
                crate::readers::dsh::title(self.store.clone(), native.to_owned()).await
            }
        };
        match found {
            Ok(title) => title,
            Err(error) => {
                tracing::debug!("session {native} has no title: {error}");
                None
            }
        }
    }
}

#[derive(Debug)]
pub struct StoreHook<T: CaptureTransport> {
    harness: StoreHarness,
    writer: CaptureWriter<T>,
    capture: StoreCapture,
    log_file: PathBuf,
}

impl<T: CaptureTransport> StoreHook<T> {
    #[must_use]
    pub fn new(
        harness: StoreHarness,
        writer: CaptureWriter<T>,
        roots: &StoreRoots,
        paths: &config::Paths,
    ) -> Option<Self> {
        Some(Self {
            harness,
            writer,
            capture: StoreCapture::new(harness, roots)?,
            log_file: paths.state_dir().join(harness.profile().log),
        })
    }

    #[must_use]
    pub fn log_file(&self) -> &Path {
        &self.log_file
    }

    pub async fn run(&self, stdin: &str, now: DateTime<Utc>) -> HookRun {
        let payload = match serde_json::from_str::<HookPayload>(stdin) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!("hook payload was not valid: {error}");
                return HookRun::done();
            }
        };
        let Some((event, trigger)) = payload
            .hook_event_name
            .as_deref()
            .and_then(|event| Some((event, self.harness.trigger(event)?)))
        else {
            return HookRun::done();
        };
        let Some(native) = payload.session_id.as_deref().filter(|id| !id.is_empty()) else {
            return HookRun::done();
        };
        let cwd = payload.cwd.as_ref().map(|path| path.display().to_string());
        let finalize = trigger == Trigger::Finalize;
        let batch = match self.capture.batch(native, event, cwd, finalize, now).await {
            Ok(Some(batch)) => batch,
            Ok(None) => return HookRun::done(),
            Err(error) => return HookRun::failed(error),
        };
        match self.writer.write(&batch, now).await {
            Ok(totals) => {
                let line = log_line(event, &batch.session_key, &totals, now);
                if let Err(error) = append_log(&self.log_file, &line).await {
                    tracing::warn!(
                        "hook log {} was not written: {error}",
                        self.log_file.display()
                    );
                }
                HookRun::done()
            }
            Err(error) => HookRun::failed(error.to_string()),
        }
    }
}

fn log_line(event: &str, session_key: &str, totals: &Totals, now: DateTime<Utc>) -> String {
    format!(
        "{} {event} {session_key}: updated conv {} — +{} msg (had {})",
        iso_at(now),
        totals.conversation_id,
        totals.inserted,
        totals.skipped
    )
}

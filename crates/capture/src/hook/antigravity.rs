use jiff::Timestamp;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;

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
use crate::readers::antigravity;
use crate::readers::antigravity::SpawnedSubagent;
use crate::readers::blocking;
use crate::records::SubagentLink;
use crate::store_path::single_component;
use crate::writer::CaptureTransport;
use crate::writer::CaptureWriter;
use crate::writer::Totals;

pub const AGENT: &str = "rivet-gemini";
pub const CHANNEL: &str = "antigravity";
const SOURCE: &str = "antigravity-hook";
const LOG_FILE: &str = "antigravity-capture.log";
const SUBAGENT_STATE_DIR: &str = "antigravity-subagents";
const DIR_MODE: u32 = 0o700;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookPayload {
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    transcript_path: Option<PathBuf>,
    #[serde(default)]
    workspace_paths: Vec<PathBuf>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    fully_idle: Option<bool>,
}

#[derive(Debug, Serialize)]
struct HookSettings<'a> {
    source: &'static str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    last_event: &'a str,
    last_ingest_at: String,
    #[serde(flatten)]
    subagent: Option<SubagentLink>,
}

#[derive(Debug)]
struct ChildState {
    dir: PathBuf,
}

impl ChildState {
    fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(SUBAGENT_STATE_DIR),
        }
    }

    fn file(&self, parent: &str, child: &str) -> Option<PathBuf> {
        (single_component(parent) && single_component(child))
            .then(|| self.dir.join(parent).join(child))
    }

    async fn holds(&self, parent: &str, child: &str, bytes: u64) -> std::io::Result<bool> {
        let Some(file) = self.file(parent, child) else {
            return Ok(false);
        };
        if !tokio::fs::try_exists(&file).await? {
            return Ok(false);
        }
        Ok(tokio::fs::read(&file).await? == bytes.to_string().as_bytes())
    }

    async fn record(&self, parent: &str, child: &str, bytes: u64) -> std::io::Result<()> {
        let Some(file) = self.file(parent, child) else {
            return Ok(());
        };
        if let Some(dir) = file.parent() {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE)).await?;
        }
        tokio::fs::write(file, bytes.to_string()).await
    }
}

struct ParentContext<'a> {
    app_dir: &'a Path,
    parent_id: &'a str,
    parent_cwd: Option<&'a str>,
    event: &'a str,
    now: Timestamp,
}

struct BatchInput<'a> {
    app_dir: &'a Path,
    native_id: &'a str,
    path: &'a Path,
    event: &'a str,
    cwd_override: Option<String>,
    model: Option<&'a str>,
    fully_idle: Option<bool>,
    spawn: Option<&'a SpawnedSubagent>,
    now: Timestamp,
}

#[derive(Debug)]
pub struct AntigravityHook<T: CaptureTransport> {
    writer: CaptureWriter<T>,
    roots: Vec<PathBuf>,
    children: ChildState,
    log_file: PathBuf,
}

impl<T: CaptureTransport> AntigravityHook<T> {
    #[must_use]
    pub fn new(writer: CaptureWriter<T>, roots: &StoreRoots, paths: &config::Paths) -> Self {
        Self {
            writer,
            roots: roots.antigravity.clone(),
            children: ChildState::new(&paths.state_dir()),
            log_file: paths.state_dir().join(LOG_FILE),
        }
    }

    #[must_use]
    pub fn log_file(&self) -> &Path {
        &self.log_file
    }

    pub async fn run(&self, stdin: &str, now: Timestamp) -> HookRun {
        let payload = match serde_json::from_str::<HookPayload>(stdin) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!("antigravity hook payload was not valid: {error}");
                return HookRun::done();
            }
        };
        let Some(event) = payload
            .hook_event_name
            .as_deref()
            .filter(|event| matches!(*event, "Stop" | "PostInvocation" | "PostToolUse"))
        else {
            return HookRun::done();
        };
        let Some(native_id) = payload
            .conversation_id
            .as_deref()
            .filter(|id| !id.is_empty() && single_component(id))
        else {
            return HookRun::done();
        };

        let (app_dir, transcript_path) = match self
            .resolve_transcript(native_id, payload.transcript_path.as_deref())
            .await
        {
            Ok(Some(found)) => found,
            Ok(None) => return HookRun::done(),
            Err(error) => return HookRun::failed(error),
        };
        let cwd = payload
            .cwd
            .as_ref()
            .or_else(|| payload.workspace_paths.first())
            .map(|path| path.display().to_string());
        let model = payload
            .model_name
            .as_deref()
            .filter(|name| !name.trim().is_empty());

        let input = BatchInput {
            app_dir: &app_dir,
            native_id,
            path: &transcript_path,
            event,
            cwd_override: cwd.clone(),
            model,
            fully_idle: payload.fully_idle,
            spawn: None,
            now,
        };
        let (batch, transcript_text, is_subagent) = match build_batch(input).await {
            Ok(Some(built)) => built,
            Ok(None) => (None, String::new(), false),
            Err(error) => return HookRun::failed(error),
        };

        let mut run = HookRun::done();
        if let Some(batch) = batch {
            self.write_and_log(event, &batch, now, None, &mut run).await;
        }
        if event == "Stop" && !is_subagent {
            let ctx = ParentContext {
                app_dir: &app_dir,
                parent_id: native_id,
                parent_cwd: cwd.as_deref(),
                event,
                now,
            };
            let sub_run = self.capture_children(&ctx, &transcript_text).await;
            run.absorb(&sub_run);
        }
        run
    }

    async fn write_and_log(
        &self,
        event: &str,
        batch: &CaptureBatchRequest,
        now: Timestamp,
        prefix: Option<&str>,
        run: &mut HookRun,
    ) -> bool {
        match self.writer.write(batch, now).await {
            Ok(totals) => {
                let line = log_line(event, &batch.session_key, &totals, now);
                if let Err(error) = append_log(&self.log_file, &line).await {
                    tracing::warn!(
                        "hook log {} was not written: {error}",
                        self.log_file.display()
                    );
                }
                true
            }
            Err(error) => {
                let mut failed = HookRun::write_failed(&error);
                if let Some(prefix) = prefix {
                    failed.stderr = format!("{prefix}: {}", failed.stderr);
                }
                run.absorb(&failed);
                error.spooled()
            }
        }
    }

    async fn resolve_transcript(
        &self,
        native_id: &str,
        hint: Option<&Path>,
    ) -> Result<Option<(PathBuf, PathBuf)>, String> {
        if let Some(hint) = hint {
            let resolved = antigravity::prefer_full_transcript(hint);
            if tokio::fs::try_exists(&resolved)
                .await
                .map_err(|error| error.to_string())?
            {
                let app_dir = antigravity::app_dir_from_transcript(&resolved)
                    .or_else(|| self.roots.first().cloned())
                    .unwrap_or_default();
                return Ok(Some((app_dir, resolved)));
            }
        }
        let roots = self.roots.clone();
        let id = native_id.to_owned();
        blocking(move || {
            for app_dir in &roots {
                if let Some(path) = antigravity::transcript_path_for(app_dir, &id)? {
                    return Ok(Some((app_dir.clone(), path)));
                }
            }
            Ok(None)
        })
        .await
        .map_err(|error| error.to_string())
    }

    async fn capture_children(&self, ctx: &ParentContext<'_>, parent_text: &str) -> HookRun {
        let spawns = antigravity::spawned_subagents(ctx.parent_id, parent_text);
        let dir = ctx.app_dir.to_path_buf();
        let pid = ctx.parent_id.to_owned();
        let db_children = match blocking(move || Ok(antigravity::child_ids_in(&dir, &pid))).await {
            Ok(ids) => ids,
            Err(error) => return HookRun::failed(error.to_string()),
        };

        let mut seen = HashSet::new();
        let mut candidates: Vec<(String, Option<SpawnedSubagent>)> = Vec::new();
        for spawn in spawns {
            if seen.insert(spawn.conversation_id.clone()) {
                candidates.push((spawn.conversation_id.clone(), Some(spawn)));
            }
        }
        for child_id in db_children {
            if seen.insert(child_id.clone()) {
                candidates.push((child_id, None));
            }
        }

        let mut run = HookRun::done();
        for (child_id, spawn) in candidates {
            if !single_component(&child_id) || child_id == ctx.parent_id {
                continue;
            }
            let child_run = self.capture_one_child(ctx, &child_id, spawn.as_ref()).await;
            run.absorb(&child_run);
        }
        run
    }

    async fn capture_one_child(
        &self,
        ctx: &ParentContext<'_>,
        child_id: &str,
        spawn: Option<&SpawnedSubagent>,
    ) -> HookRun {
        let dir = ctx.app_dir.to_path_buf();
        let cid = child_id.to_owned();
        let path = match blocking(move || antigravity::transcript_path_for(&dir, &cid)).await {
            Ok(Some(path)) => path,
            Ok(None) => return HookRun::done(),
            Err(error) => return HookRun::failed(format!("subagent {child_id}: {error}")),
        };
        let bytes = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta.len(),
            Err(error) => {
                return HookRun::failed(format!("subagent {child_id} was not read: {error}"));
            }
        };
        match self.children.holds(ctx.parent_id, child_id, bytes).await {
            Ok(true) => return HookRun::done(),
            Ok(false) => {}
            Err(error) => {
                return HookRun::failed(format!("subagent {child_id} was not checked: {error}"));
            }
        }
        let input = BatchInput {
            app_dir: ctx.app_dir,
            native_id: child_id,
            path: &path,
            event: ctx.event,
            cwd_override: ctx.parent_cwd.map(str::to_owned),
            model: None,
            fully_idle: None,
            spawn,
            now: ctx.now,
        };
        let built = match build_batch(input).await {
            Ok(Some((Some(batch), _, _))) => batch,
            Ok(Some((None, _, _)) | None) => return HookRun::done(),
            Err(error) => return HookRun::failed(format!("subagent {child_id}: {error}")),
        };
        let mut run = HookRun::done();
        let prefix = format!("subagent {child_id}");
        let kept = self
            .write_and_log(ctx.event, &built, ctx.now, Some(&prefix), &mut run)
            .await;
        if kept && let Err(error) = self.children.record(ctx.parent_id, child_id, bytes).await {
            tracing::warn!("subagent {child_id} was not recorded: {error}");
        }
        run
    }
}

pub(crate) async fn backfill_batch(
    app_dir: &Path,
    native_id: &str,
    path: &Path,
    event: &str,
    now: Timestamp,
) -> Result<Option<CaptureBatchRequest>, String> {
    let input = BatchInput {
        app_dir,
        native_id,
        path,
        event,
        cwd_override: None,
        model: None,
        fully_idle: None,
        spawn: None,
        now,
    };
    let built = build_batch(input).await?;
    Ok(built.and_then(|(batch, _, _)| batch))
}

async fn build_batch(
    input: BatchInput<'_>,
) -> Result<Option<(Option<CaptureBatchRequest>, String, bool)>, String> {
    let dir = input.app_dir.to_path_buf();
    let id = input.native_id.to_owned();
    let file = input.path.to_path_buf();
    let event_name = input.event.to_owned();
    let cwd_override = input.cwd_override;
    let model_name = input.model.map(str::to_owned);
    let fully_idle = input.fully_idle;
    let spawn = input.spawn.cloned();
    let now = input.now;

    blocking(move || {
        let (resolved_path, text, slice) = antigravity::capture_file(&id, &file)?;
        let summary = antigravity::summary_in(&dir, &id);
        let subagent = antigravity::subagent_link_for(&dir, &id, &text, &summary, spawn.as_ref());
        let is_subagent = subagent.is_some();
        if slice.messages.is_empty() {
            return Ok(Some((None, text, is_subagent)));
        }
        let session = SessionId::new(HarnessId::Antigravity, &id);
        let cwd = cwd_override.or_else(|| summary.cwd.clone());
        let subagent_idle = is_subagent
            && (event_name == "Stop" && fully_idle != Some(false)
                || summary.status.as_deref() == Some("CASCADE_RUN_STATUS_IDLE"));
        let settings = JsonObject::from_serialize(&HookSettings {
            source: SOURCE,
            session_id: &id,
            file: Some(resolved_path.display().to_string()),
            cwd,
            model: model_name.as_deref(),
            last_event: &event_name,
            last_ingest_at: iso_at(now),
            subagent,
        })
        .map_err(|error| kernel::transcript::TranscriptReaderError::Read {
            message: format!("antigravity settings JSON failed: {error}"),
            source: Box::new(error),
        })?;
        let title = summary
            .title
            .or_else(|| spawn.and_then(|item| item.description));
        let batch = CaptureBatchRequest {
            session_key: session.to_string(),
            agent: AGENT.to_owned(),
            channel: Some(CHANNEL.to_owned()),
            title,
            settings: Some(settings),
            task_id: None,
            finalize: subagent_idle.then_some(true),
            created_at: None,
            updated_at: None,
            messages: slice.messages,
        };
        Ok(Some((Some(batch), text, is_subagent)))
    })
    .await
    .map_err(|error| error.to_string())
}

fn log_line(event: &str, session_key: &str, totals: &Totals, now: Timestamp) -> String {
    format!(
        "{} {event} {session_key}: updated conv {} — +{} msg (had {})",
        iso_at(now),
        totals.conversation_id,
        totals.inserted,
        totals.skipped
    )
}

use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::HookError;
use super::iso_at;
use super::transcript_batch;
use crate::listing::ClaudeFile;
use crate::records::SOURCE;
use crate::records::SubagentLink;
use crate::records::TranscriptSettings;
use crate::transcript::parse_lines;
use crate::transcript::read_uncapped_async;
use crate::transcript::report_skipped;

pub(crate) struct FileCapture<'a> {
    pub(crate) file: &'a ClaudeFile,
    pub(crate) last_event: &'a str,
    pub(crate) finalize: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) agent_type: Option<&'a str>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    agent_type: Option<String>,
    description: Option<String>,
    tool_use_id: Option<String>,
}

pub(crate) async fn batch(
    capture: &FileCapture<'_>,
    now: DateTime<Utc>,
) -> Result<Option<CaptureBatchRequest>, HookError> {
    let file = capture.file;
    let text = read_uncapped_async(&file.path).await?;
    let parsed = parse_lines(&text);
    report_skipped(&parsed.skipped);
    let session = SessionId::new(HarnessId::ClaudeCode, &file.id);
    let mut batch = transcript_batch::build(
        Some(&file.path),
        session.as_str(),
        &parsed.lines,
        capture.finalize,
        &[],
    )?;
    if batch.messages.is_empty() {
        return Ok(None);
    }
    let subagent = match &file.parent {
        Some(parent) => {
            let meta = meta(&file.path).await;
            let description = meta
                .description
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty());
            if description.is_some() {
                batch.title.clone_from(&description);
            }
            Some(SubagentLink {
                parent_session_id: parent.clone(),
                agent_id: file.id.clone(),
                agent_type: capture.agent_type.map(str::to_owned).or(meta.agent_type),
                description,
                tool_use_id: meta.tool_use_id,
            })
        }
        None => None,
    };
    let cwd = capture
        .cwd
        .clone()
        .or_else(|| parsed.lines.iter().find_map(|line| line.cwd.clone()));
    batch.settings = Some(JsonObject::from_serialize(&TranscriptSettings {
        source: SOURCE,
        file: Some(file.path.display().to_string()),
        session_id: file.id.clone(),
        pr_url: None,
        cwd,
        last_event: capture.last_event,
        last_ingest_at: iso_at(now),
        subagent,
    })?);
    Ok(Some(batch))
}

async fn meta(transcript: &Path) -> Meta {
    let path = transcript.with_extension("meta.json");
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a subagent whose meta file is absent or unreadable is still captured, without the link fields (CONTRACT note 148)"
        )
    )]
    let found: Option<Meta> = tokio::fs::read(&path)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    found.unwrap_or_default()
}

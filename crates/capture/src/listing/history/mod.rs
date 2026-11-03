mod claude;
mod display;
mod grok;
mod tools;

use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::StoreTranscript;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use super::StoreRoots;
use crate::readers::StoreReader;
use crate::readers::blocking;

pub(super) async fn transcript(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
    window: TranscriptWindow,
) -> Result<TranscriptRead, TranscriptReaderError> {
    if matches!(harness, HarnessId::Cowork | HarnessId::Dsh) {
        return Ok(TranscriptRead::NoSource);
    }
    let Some(root) = roots.single(harness).map(Path::to_path_buf) else {
        return Ok(TranscriptRead::Read(StoreTranscript::default()));
    };
    let native = native.to_owned();
    if harness == HarnessId::ClaudeCode {
        return blocking(move || claude::transcript(&root, &native, window))
            .await
            .map(TranscriptRead::Read);
    }
    if matches!(window, TranscriptWindow::Before(_)) {
        return Ok(TranscriptRead::Read(StoreTranscript::default()));
    }
    if harness == HarnessId::GrokBuild {
        return blocking(move || grok::transcript(&root, &native))
            .await
            .map(TranscriptRead::Read);
    }
    text_turns(harness, root, &native).await
}

pub(super) async fn tool_output(
    roots: &StoreRoots,
    harness: HarnessId,
    native: &str,
    call: &str,
) -> Result<ToolOutputRead, TranscriptReaderError> {
    if harness != HarnessId::ClaudeCode {
        return Ok(ToolOutputRead::NoSource);
    }
    let Some(root) = roots.single(harness).map(Path::to_path_buf) else {
        return Ok(ToolOutputRead::Missing);
    };
    let native = native.to_owned();
    let call = call.to_owned();
    blocking(move || claude::tool_output(&root, &native, &call))
        .await
        .map(|found| found.map_or(ToolOutputRead::Missing, ToolOutputRead::Found))
}

async fn text_turns(
    harness: HarnessId,
    root: PathBuf,
    native: &str,
) -> Result<TranscriptRead, TranscriptReaderError> {
    let Some(reader) = StoreReader::open(harness, root) else {
        return Ok(TranscriptRead::NoSource);
    };
    let turns = reader.read(&SessionId::new(harness, native)).await?;
    Ok(TranscriptRead::Read(StoreTranscript {
        turns: turns.into_iter().filter_map(text_turn).collect(),
        earlier: None,
        context: None,
    }))
}

fn text_turn(turn: Turn) -> Option<HarnessTranscriptTurn> {
    let role = match turn.role {
        Role::User => SessionMessageRole::User,
        Role::Assistant => SessionMessageRole::Assistant,
        Role::System | Role::Tool => return None,
    };
    let at = turn.at.timestamp_millis();
    Some(HarnessTranscriptTurn {
        role,
        text: turn.content,
        thinking: None,
        tools: None,
        usage: None,
        model: None,
        stop_reason: None,
        last_block: None,
        attachments: None,
        complete: false,
        compact: false,
        started_at_ms: Some(at),
        ended_at_ms: Some(at),
    })
}

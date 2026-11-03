#![cfg(test)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::fmt::Debug;
use strum::IntoEnumIterator;
use wire::events::ContextSource;
use wire::gateway::sessions::{
    HarnessSessionResponse, HarnessSessionTranscriptResponse, HarnessTranscriptTool,
    HarnessTranscriptTurn, HarnessTurnAccepted, MessageUsage, SessionMessageRole, SessionWsFrame,
    TranscriptBlock, TranscriptResponse, TranscriptToolStatus, TranscriptWsFrame,
};
use wire::harness::{
    HarnessCode, HarnessEvent, HarnessId, HarnessSessionStatus, HarnessSessionSummary,
    HarnessStoreSession, HarnessStoreSessionList, RosterCommand,
};
use wire::session_id::{
    DecodedSegment, SegmentError, SessionId, SessionRef, decode_segment, encode_segment,
};

const UUID: &str = "3f2c9a10-1b2c-4d5e-8f90-a1b2c3d4e5f6";

fn session(text: &str) -> SessionId {
    text.parse().unwrap()
}

fn roundtrip<T>(wire: &Value) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let typed: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(&serde_json::to_value(&typed).unwrap(), wire);
    let again: T = serde_json::from_value(serde_json::to_value(&typed).unwrap()).unwrap();
    assert_eq!(again, typed);
    typed
}

fn invalid(segment: &str) {
    assert_eq!(
        decode_segment(segment),
        Err(SegmentError {
            segment: segment.to_owned()
        })
    );
}

#[test]
fn t0_1_encode_is_unpadded_base64url_and_decodes_back() {
    let id = session(&format!("claude-code:{UUID}"));
    let encoded = encode_segment(&id);
    assert_eq!(
        encoded,
        "Y2xhdWRlLWNvZGU6M2YyYzlhMTAtMWIyYy00ZDVlLThmOTAtYTFiMmMzZDRlNWY2"
    );
    let decoded = decode_segment(&encoded).unwrap();
    assert_eq!(decoded.session, SessionRef::Canonical(id.clone()));
    assert_eq!(decoded.redirected_to(&id), None);
}

#[test]
fn session_id_exposes_harness_and_native_parts() {
    let parsed = session("kimi-code:session_a:b");
    assert_eq!(parsed.harness(), HarnessId::KimiCode);
    assert_eq!(parsed.native(), "session_a:b");
    let built = SessionId::new(HarnessId::KimiCode, "session_a:b");
    assert_eq!(built, parsed);
    assert_eq!(
        serde_json::to_value(&built).unwrap(),
        json!("kimi-code:session_a:b")
    );
}

#[test]
fn encode_uses_the_url_alphabet_without_padding() {
    let id = session("hermes:a?>~");
    let encoded = encode_segment(&id);
    assert_eq!(encoded, "aGVybWVzOmE_Pn4");
    assert_eq!(
        decode_segment(&encoded).unwrap().session,
        SessionRef::Canonical(id)
    );
}

#[test]
fn t0_2_decodes_an_unpadded_hermes_segment() {
    let decoded = decode_segment("aGVybWVzOjIwMjYwODAyXzIyNTY0N182YWQwYjk").unwrap();
    assert_eq!(
        decoded,
        DecodedSegment {
            requested: "hermes:20260802_225647_6ad0b9".to_owned(),
            session: SessionRef::Canonical(session("hermes:20260802_225647_6ad0b9")),
        }
    );
}

#[test]
fn t0_3_a_bare_uuid_is_not_misread_as_base64() {
    let decoded = decode_segment(UUID).unwrap();
    assert_eq!(decoded.session, SessionRef::Bare(UUID.to_owned()));
    let resolved = session(&format!("claude-code:{UUID}"));
    assert_eq!(decoded.redirected_to(&resolved), Some(resolved));
}

#[test]
fn bare_uuid_match_is_case_insensitive() {
    let upper = UUID.to_uppercase();
    assert_eq!(
        decode_segment(&upper).unwrap().session,
        SessionRef::Bare(upper)
    );
}

#[test]
fn t0_4_leading_whitespace_in_the_decoded_id_is_rejected() {
    invalid("IGNsYXVkZS1jb2RlOng");
}

#[test]
fn t0_5_a_roster_token_prefix_is_rejected() {
    invalid("Y2xhdWRlOmFiYw");
}

#[test]
fn t0_6_path_fallback_collapses_to_the_uuid_and_redirects() {
    let decoded = decode_segment(
        "Y2xhdWRlLWNvZGU6LWhvbWUtcml2ZXQvM2YyYzlhMTAtMWIyYy00ZDVlLThmOTAtYTFiMmMzZDRlNWY2",
    )
    .unwrap();
    let canonical = session(&format!("claude-code:{UUID}"));
    assert_eq!(decoded.session, SessionRef::Canonical(canonical.clone()));
    assert_eq!(decoded.redirected_to(&canonical), Some(canonical));
}

#[test]
fn a_native_with_a_slash_but_no_uuid_tail_is_kept() {
    let id = session("grok-build:a/b");
    let decoded = decode_segment(&encode_segment(&id)).unwrap();
    assert_eq!(decoded.session, SessionRef::Canonical(id.clone()));
    assert_eq!(decoded.redirected_to(&id), None);
}

#[test]
fn t0_7_padding_is_rejected() {
    invalid("abc=");
}

#[test]
fn t0_8_a_task_key_is_rejected() {
    invalid("task%3A123");
    invalid("task:123");
}

#[test]
fn empty_and_whitespace_segments_are_rejected() {
    invalid("");
    invalid("%20");
    invalid(&encode_segment_text(" "));
}

#[test]
fn a_length_one_mod_four_segment_is_not_base64() {
    invalid("abcde");
}

#[test]
fn a_percent_encoded_segment_is_decoded_first() {
    let id = session(&format!("codex:{UUID}"));
    let escaped = encode_segment(&id).replace('Y', "%59");
    assert_eq!(
        decode_segment(&escaped).unwrap().session,
        SessionRef::Canonical(id)
    );
    let bare = UUID.replace('-', "%2D");
    assert_eq!(
        decode_segment(&bare).unwrap().session,
        SessionRef::Bare(UUID.to_owned())
    );
}

#[test]
fn a_segment_that_is_not_utf8_after_percent_decoding_is_read_raw() {
    invalid("%FF");
}

#[test]
fn base64_of_non_utf8_bytes_falls_through_to_rejection() {
    invalid("__8");
}

fn encode_segment_text(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(text)
}

#[test]
fn roster_commands_map_to_harness_ids_both_ways() {
    let table = [
        (HarnessId::ClaudeCode, "claude"),
        (HarnessId::GrokBuild, "grok"),
        (HarnessId::KimiCode, "kimi"),
        (HarnessId::Hermes, "hermes"),
        (HarnessId::Codex, "codex"),
        (HarnessId::Opencode, "opencode"),
        (HarnessId::Omp, "omp"),
        (HarnessId::Dsh, "dsh"),
        (HarnessId::Cursor, "cursor"),
        (HarnessId::Cowork, "cowork"),
    ];
    assert_eq!(table.len(), HarnessId::iter().count());
    for (harness, command) in table {
        let roster = RosterCommand::from(harness);
        assert_eq!(roster.to_string(), command);
        assert_eq!(serde_json::to_value(roster).unwrap(), json!(command));
        assert_eq!(command.parse::<RosterCommand>().unwrap(), roster);
        assert_eq!(HarnessId::from(roster), harness);
    }
}

#[test]
fn harness_codes_carry_status_and_retryable() {
    let table = [
        (
            HarnessCode::InvalidSessionId,
            "invalid_session_id",
            400,
            false,
        ),
        (HarnessCode::BadRequest, "bad_request", 400, false),
        (
            HarnessCode::SessionIdCollision,
            "session_id_collision",
            409,
            false,
        ),
        (HarnessCode::TurnInFlight, "turn_in_flight", 409, true),
        (
            HarnessCode::CapabilityUnsupported,
            "capability_unsupported",
            501,
            false,
        ),
        (HarnessCode::UnknownApproval, "unknown_approval", 404, false),
        (HarnessCode::UnknownPrompt, "unknown_prompt", 404, false),
        (HarnessCode::TurnFailed, "turn_failed", 502, false),
        (
            HarnessCode::SessionRunningElsewhere,
            "session_running_elsewhere",
            409,
            false,
        ),
    ];
    assert_eq!(table.len(), HarnessCode::iter().count());
    for (code, name, status, retryable) in table {
        assert_eq!(roundtrip::<HarnessCode>(&json!(name)), code);
        assert_eq!(code.to_string(), name);
        assert_eq!(code.status(), status);
        assert_eq!(code.retryable(), retryable);
    }
}

#[test]
fn b5_1_turns_serialize_with_optional_fields_omitted() {
    let wire = json!([
        {"role": "user", "text": "hi"},
        {
            "role": "assistant",
            "text": "hello",
            "thinking": "x",
            "usage": {"promptTokens": 15, "completionTokens": 3, "cachedTokens": 5},
            "stopReason": "end_turn",
            "lastBlock": "text",
            "complete": true
        }
    ]);
    let turns: Vec<HarnessTranscriptTurn> = roundtrip(&wire);
    assert_eq!(
        turns,
        vec![
            HarnessTranscriptTurn {
                role: SessionMessageRole::User,
                text: "hi".to_owned(),
                thinking: None,
                tools: None,
                usage: None,
                model: None,
                stop_reason: None,
                last_block: None,
                attachments: None,
                complete: false,
                compact: false,
                started_at_ms: None,
                ended_at_ms: None,
            },
            HarnessTranscriptTurn {
                role: SessionMessageRole::Assistant,
                text: "hello".to_owned(),
                thinking: Some("x".to_owned()),
                tools: None,
                usage: Some(MessageUsage {
                    prompt_tokens: 15,
                    completion_tokens: 3,
                    cached_tokens: 5,
                }),
                model: None,
                stop_reason: Some("end_turn".to_owned()),
                last_block: Some(TranscriptBlock::Text),
                attachments: None,
                complete: true,
                compact: false,
                started_at_ms: None,
                ended_at_ms: None,
            },
        ]
    );
}

#[test]
fn b5_2_tools_carry_status_args_and_id() {
    let wire = json!({
        "role": "assistant",
        "text": "done",
        "tools": [{"name": "Bash", "status": "done", "args": {"command": "ls"}, "id": "t1"}],
        "stopReason": "end_turn",
        "lastBlock": "text",
        "complete": true
    });
    let turn: HarnessTranscriptTurn = roundtrip(&wire);
    let tools = turn.tools.unwrap();
    assert_eq!(tools.first().unwrap().status, TranscriptToolStatus::Done);
    assert_eq!(
        tools.first().unwrap().args.as_ref().unwrap().get("command"),
        Some(&json!("ls"))
    );
}

#[test]
fn every_block_and_tool_status_has_its_wire_name() {
    for (block, name) in [
        (TranscriptBlock::Thinking, "thinking"),
        (TranscriptBlock::Text, "text"),
        (TranscriptBlock::ToolUse, "tool_use"),
        (TranscriptBlock::ToolResult, "tool_result"),
    ] {
        assert_eq!(roundtrip::<TranscriptBlock>(&json!(name)), block);
    }
    for (status, name) in [
        (TranscriptToolStatus::Running, "running"),
        (TranscriptToolStatus::Done, "done"),
        (TranscriptToolStatus::Error, "error"),
    ] {
        assert_eq!(roundtrip::<TranscriptToolStatus>(&json!(name)), status);
    }
}

#[test]
fn prompt_tools_carry_input_and_result_text() {
    let wire = json!({
        "name": "AskUserQuestion",
        "status": "error",
        "id": "toolu_1",
        "input": {"truncated": true},
        "resultText": "Yes"
    });
    let tool: HarnessTranscriptTool = roundtrip(&wire);
    assert_eq!(tool.status, TranscriptToolStatus::Error);
    assert_eq!(tool.result_text.as_deref(), Some("Yes"));
}

#[test]
fn b5_6_compact_markers_keep_the_compact_flag() {
    let wire = json!({
        "role": "assistant",
        "text": "Conversation compacted (180k tokens → 12k tokens)",
        "usage": {"promptTokens": 12000, "completionTokens": 0, "cachedTokens": 0},
        "stopReason": "end_turn",
        "lastBlock": "text",
        "complete": true,
        "compact": true
    });
    let turn: HarnessTranscriptTurn = roundtrip(&wire);
    assert!(turn.compact);
}

#[test]
fn transcript_response_carries_the_context_stamp() {
    let wire = json!({
        "sessionId": format!("claude-code:{UUID}"),
        "harnessId": "claude-code",
        "turns": [{"role": "user", "text": "hi"}],
        "contextWindow": 1_000_000,
        "compactAt": 965_000,
        "contextSource": "observed",
        "redirectedTo": format!("claude-code:{UUID}")
    });
    let response: HarnessSessionTranscriptResponse = roundtrip(&wire);
    assert_eq!(response.compact_at, 965_000);
    assert_eq!(response.context_source, ContextSource::Observed);
    let without_redirect = json!({
        "sessionId": "hermes:x",
        "harnessId": "hermes",
        "turns": [],
        "contextWindow": 262_144,
        "compactAt": 227_144,
        "contextSource": "default"
    });
    roundtrip::<HarnessSessionTranscriptResponse>(&without_redirect);
}

#[test]
fn legacy_transcript_response_compact_at_is_a_token_count() {
    let wire = json!({
        "id": "claude-code:x",
        "command": "claude",
        "turns": [],
        "contextWindow": 200_000,
        "compactAt": 165_000,
        "contextSource": "spawn"
    });
    let response: TranscriptResponse = roundtrip(&wire);
    assert_eq!(response.compact_at, Some(165_000));
}

#[test]
fn turn_accepted_omits_absent_fields() {
    let minimal: HarnessTurnAccepted =
        roundtrip(&json!({"ok": true, "sessionId": "claude-code:x"}));
    assert!(!minimal.dismissed_dialog);
    let full: HarnessTurnAccepted = roundtrip(&json!({
        "ok": true,
        "sessionId": format!("claude-code:{UUID}"),
        "redirectedTo": format!("claude-code:{UUID}"),
        "dismissedDialog": true
    }));
    assert!(full.dismissed_dialog);
}

fn summary_wire() -> Value {
    json!({
        "sessionId": format!("claude-code:{UUID}"),
        "harnessId": "claude-code",
        "title": "fix the build",
        "cwd": "/home/user/proj",
        "createdAt": "2025-10-17T11:20:00.000Z",
        "updatedAt": "2025-10-17T11:20:00.000Z",
        "status": "ended",
        "parentSessionId": "claude-code:p",
        "agentName": "explore",
        "model": "claude-opus-4",
        "taskId": "t"
    })
}

#[test]
fn session_response_flattens_the_summary_beside_redirected_to() {
    let mut wire = summary_wire();
    wire.as_object_mut().unwrap().insert(
        "redirectedTo".to_owned(),
        json!(format!("claude-code:{UUID}")),
    );
    let response: HarnessSessionResponse = roundtrip(&wire);
    assert!(response.summary.extra.is_empty());
    assert_eq!(response.summary.status, HarnessSessionStatus::Ended);
    assert_eq!(
        response.redirected_to,
        Some(session(&format!("claude-code:{UUID}")))
    );
    roundtrip::<HarnessSessionResponse>(&summary_wire());
}

#[test]
fn session_created_carries_a_summary_object() {
    let wire = json!({
        "type": "session-created",
        "sessionId": format!("claude-code:{UUID}"),
        "summary": summary_wire()
    });
    let event: HarnessEvent = roundtrip(&wire);
    let HarnessEvent::SessionCreated { summary, .. } = event else {
        panic!("expected session-created, got {event:?}");
    };
    let expected: HarnessSessionSummary = serde_json::from_value(summary_wire()).unwrap();
    assert_eq!(*summary, expected);
}

#[test]
fn session_updated_requires_status() {
    let wire = json!({
        "type": "session-updated",
        "sessionId": "claude-code:b",
        "status": "idle",
        "previousSessionId": "claude-code:a",
        "supersedes": "claude-code:a",
        "blocked": true
    });
    let event: HarnessEvent = roundtrip(&wire);
    assert!(matches!(
        event,
        HarnessEvent::SessionUpdated {
            status: HarnessSessionStatus::Idle,
            blocked: Some(true),
            ..
        }
    ));
    roundtrip::<HarnessEvent>(
        &json!({"type": "session-updated", "sessionId": "claude-code:b", "status": "ended"}),
    );
    assert!(
        serde_json::from_value::<HarnessEvent>(
            json!({"type": "session-updated", "sessionId": "claude-code:b"})
        )
        .is_err()
    );
}

#[test]
fn transcript_event_carries_turns() {
    let wire = json!({
        "type": "transcript",
        "sessionId": "claude-code:x",
        "rev": 3,
        "from": 0,
        "total": 1,
        "turns": [{"role": "user", "text": "hi"}],
        "command": "claude",
        "truncatedBefore": true,
        "earlier": 1_048_576,
        "contextUsed": 52_640,
        "contextWindow": 200_000,
        "compactAt": 165_000,
        "contextSource": "default"
    });
    let event: HarnessEvent = roundtrip(&wire);
    let HarnessEvent::Transcript { turns, command, .. } = event else {
        panic!("expected transcript, got {event:?}");
    };
    assert_eq!(turns.len(), 1);
    assert_eq!(command, RosterCommand::Claude);
    roundtrip::<HarnessEvent>(&json!({
        "type": "transcript",
        "sessionId": "hermes:x",
        "rev": 1,
        "from": 0,
        "total": 0,
        "turns": [],
        "command": "hermes"
    }));
}

#[test]
fn status_event_is_the_status_frame() {
    let wire = json!({
        "type": "status",
        "sessionId": "claude-code:x",
        "status": "working",
        "since": 1_760_700_000_000_i64,
        "source": "protocol",
        "phase": "tool",
        "tool": {"name": "Bash", "toolCallId": "t1"}
    });
    let event: HarnessEvent = roundtrip(&wire);
    assert!(matches!(event, HarnessEvent::Status(_)));
    let text = serde_json::to_string(&event).unwrap();
    assert_eq!(text.matches("\"type\"").count(), 1);
}

#[test]
fn prompt_event_carries_questions() {
    let wire = json!({
        "type": "prompt",
        "sessionId": "claude-code:x",
        "promptId": "p1",
        "kind": "ask-user",
        "toolName": "AskUserQuestion",
        "questions": [
            {
                "question": "Which?",
                "header": "Pick",
                "multiSelect": false,
                "freeText": true,
                "options": [{"label": "A", "description": "first"}, {"label": "B"}]
            },
            {"multiSelect": true, "options": []}
        ],
        "resolved": {"at": 1_760_700_000_000_i64, "answerText": "A"},
        "screen": {"current": 1, "total": 2}
    });
    let event: HarnessEvent = roundtrip(&wire);
    let HarnessEvent::Prompt {
        questions,
        resolved,
        ..
    } = event
    else {
        panic!("expected prompt, got {event:?}");
    };
    assert_eq!(questions.first().unwrap().options.len(), 2);
    assert_eq!(
        resolved.unwrap().at,
        DateTime::<Utc>::from_timestamp_millis(1_760_700_000_000).unwrap()
    );
    roundtrip::<HarnessEvent>(&json!({
        "type": "prompt",
        "sessionId": "claude-code:x",
        "promptId": "p2",
        "kind": "ask-user",
        "toolName": "AskUserQuestion",
        "questions": []
    }));
}

#[test]
fn transcript_ws_frame_turns_are_turn_objects() {
    let wire = json!({
        "kind": "transcript",
        "session": "abc",
        "rev": 2,
        "from": 1,
        "turns": [{"role": "assistant", "text": "ok", "complete": true}],
        "total": 2,
        "command": "claude"
    });
    let frame: SessionWsFrame = roundtrip(&wire);
    let SessionWsFrame::Transcript(TranscriptWsFrame { turns, .. }) = &frame else {
        panic!("expected transcript, got {frame:?}");
    };
    assert!(turns.first().unwrap().complete);
    let text = serde_json::to_string(&frame).unwrap();
    assert_eq!(text.matches("\"kind\"").count(), 1);
}

#[test]
fn b2_5_legacy_rows_use_id_and_integer_updated_at() {
    let wire = json!({
        "sessions": [
            {
                "id": UUID,
                "command": "claude",
                "title": "fix the build",
                "updatedAt": 1_760_700_000_000_i64,
                "parentSessionId": "p",
                "agentName": "explore",
                "model": "claude-opus-4",
                "taskId": "t"
            },
            {"id": "h", "command": "hermes", "title": "h", "updatedAt": 0}
        ]
    });
    let list: HarnessStoreSessionList = roundtrip(&wire);
    assert_eq!(
        list.sessions.first().unwrap().command,
        RosterCommand::Claude
    );
    assert_eq!(
        list.sessions.get(1).unwrap().updated_at,
        DateTime::<Utc>::UNIX_EPOCH
    );
}

#[test]
fn legacy_rows_never_emit_created_at() {
    let row = HarnessStoreSession {
        id: "x".to_owned(),
        command: RosterCommand::Omp,
        title: "x".to_owned(),
        updated_at: DateTime::<Utc>::from_timestamp_millis(1_760_700_000_123).unwrap(),
        created_at: Some(DateTime::<Utc>::UNIX_EPOCH),
        parent_session_id: None,
        agent_name: None,
        model: None,
        task_id: None,
    };
    assert_eq!(
        serde_json::to_value(&row).unwrap(),
        json!({"id": "x", "command": "omp", "title": "x", "updatedAt": 1_760_700_000_123_i64})
    );
}

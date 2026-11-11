#![cfg(test)]
use crate::session_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gateway::sessions::{SessionRegistry, owners_file};
use kernel::transcript::{HarnessTranscriptTurn, MessageUsage, SessionMessageRole};
use kernel::transcript::{
    ListLimit, SessionLister, SessionRenameError, StoreSession, StoreTranscript, ToolOutputRead,
    TranscriptRead, TranscriptReaderError, TranscriptWindow,
};
use parking_lot::Mutex;
use session_support::{Fixture, http, user};
use strum::IntoEnumIterator as _;
use wire::events::ContextSource;
use wire::gateway::GatewayError;
use wire::gateway::sessions::{
    HarnessSessionResponse, HarnessSessionTranscriptResponse, SessionPreviewKind,
    SessionPreviewList,
};
use wire::harness::{HarnessId, HarnessSessionList, StoreHarnessList};
use wire::session_id::{SessionId, encode_segment};

const U1: &str = "3f2c9a10-1b2c-4d5e-8f90-a1b2c3d4e5f6";
const U2: &str = "00000000-0000-4000-8000-000000000002";
const ALICE: Option<&str> = Some("alice-test-token");

#[derive(Default)]
struct Stores {
    rows: Vec<StoreSession>,
    transcripts: BTreeMap<(HarnessId, String), Vec<HarnessTranscriptTurn>>,
    cwd: BTreeMap<String, String>,
    limits: Mutex<Vec<usize>>,
    later: Mutex<BTreeMap<String, Vec<HarnessTranscriptTurn>>>,
    elsewhere: BTreeMap<String, u32>,
}

#[async_trait]
impl SessionLister for Stores {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
        self.limits.lock().push(limit.get());
        let mut rows: Vec<StoreSession> = self
            .rows
            .iter()
            .filter(|row| harnesses.contains(&row.harness))
            .cloned()
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        rows.truncate(limit.get());
        rows
    }

    async fn describe(
        &self,
        harness: HarnessId,
        native: &str,
        _parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        Ok(self
            .rows
            .iter()
            .find(|row| row.harness == harness && row.id == native)
            .cloned())
    }

    async fn store_cwd(
        &self,
        _harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(self.cwd.get(native).cloned())
    }

    async fn running_elsewhere(
        &self,
        _harness: HarnessId,
        native: &str,
    ) -> Result<Option<u32>, TranscriptReaderError> {
        Ok(self.elsewhere.get(native).copied())
    }

    async fn transcript(
        &self,
        harness: HarnessId,
        native: &str,
        _window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        if matches!(harness, HarnessId::Cowork | HarnessId::Dsh) {
            return Ok(TranscriptRead::NoSource);
        }
        Ok(TranscriptRead::Read(StoreTranscript {
            turns: self
                .transcripts
                .get(&(harness, native.to_owned()))
                .cloned()
                .or_else(|| self.later.lock().get(native).cloned())
                .unwrap_or_default(),
            earlier: None,
            context: None,
        }))
    }

    async fn rename(
        &self,
        harness: HarnessId,
        _native: &str,
        _title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        Err(SessionRenameError::Unsupported(harness))
    }

    async fn tool_output(
        &self,
        _harness: HarnessId,
        _native: &str,
        _tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        Ok(ToolOutputRead::NoSource)
    }
}

fn row(harness: HarnessId, id: &str, updated_at: i64) -> StoreSession {
    StoreSession::new(harness, id, updated_at)
}

fn titled(mut row: StoreSession, title: &str) -> StoreSession {
    title.clone_into(&mut row.title);
    row
}

fn child(mut row: StoreSession, parent: &str) -> StoreSession {
    row.parent_session_id = Some(parent.to_owned());
    row
}

fn turn(role: SessionMessageRole, text: &str) -> HarnessTranscriptTurn {
    HarnessTranscriptTurn {
        role,
        text: text.to_owned(),
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
    }
}

fn answered(text: &str, model: &str, prompt_tokens: u64) -> HarnessTranscriptTurn {
    HarnessTranscriptTurn {
        model: Some(model.to_owned()),
        usage: Some(MessageUsage {
            prompt_tokens,
            completion_tokens: 1,
            cached_tokens: 0,
        }),
        ..turn(SessionMessageRole::Assistant, text)
    }
}

struct Setup {
    fixture: Fixture,
    lister: Arc<Stores>,
    _state: tempfile::TempDir,
}

fn setup(owners: &str, stores: Stores) -> Setup {
    let state = tempfile::tempdir().expect("state");
    let path = owners_file(state.path());
    std::fs::create_dir_all(path.parent().expect("owners directory")).expect("owners directory");
    std::fs::write(&path, owners).expect("owners");
    let registry = SessionRegistry::default()
        .with_owners_file(path)
        .with_node_owner(user("owner"));
    let lister = Arc::new(stores);
    let fixture = Fixture::with_lister(registry, Some(lister.clone()));
    Setup {
        fixture,
        lister,
        _state: state,
    }
}

async fn get(setup: &Setup, path: &str, token: Option<&str>) -> (u16, Vec<u8>) {
    http(&setup.fixture.gateway, "GET", path, Vec::new(), token).await
}

fn parsed<T: serde::de::DeserializeOwned>(body: &[u8]) -> T {
    serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(body)))
}

fn error(body: &[u8]) -> GatewayError {
    parsed(body)
}

fn enc(session: &str) -> String {
    encode_segment(&session.parse::<SessionId>().expect("session id"))
}

fn plane_ids(body: &[u8]) -> Vec<String> {
    parsed::<HarnessSessionList>(body)
        .sessions
        .into_iter()
        .map(|row| row.session_id.native().to_owned())
        .collect()
}

#[tokio::test]
async fn every_harness_is_listed_and_lists_sessions_only_with_a_store_reader() {
    let bare = Fixture::new();
    let (status, body) = http(&bare.gateway, "GET", "/api/harnesses", Vec::new(), None).await;
    assert_eq!(status, 200);
    let listed: StoreHarnessList = parsed(&body);
    let ids: Vec<HarnessId> = listed.harnesses.iter().map(|row| row.harness_id).collect();
    assert_eq!(ids, HarnessId::iter().collect::<Vec<_>>());
    assert!(
        listed
            .harnesses
            .iter()
            .all(|row| !row.capabilities.list_sessions)
    );
    let setup = setup("{}", Stores::default());
    let (_, body) = get(&setup, "/api/harnesses", None).await;
    let listed: StoreHarnessList = parsed(&body);
    assert!(
        listed
            .harnesses
            .iter()
            .all(|row| row.capabilities.list_sessions)
    );
}

fn claude(native: &str) -> SessionId {
    SessionId::new(HarnessId::ClaudeCode, native)
}

#[tokio::test]
async fn listed_children_inherit_their_parent_owner() {
    let stores = Stores {
        rows: vec![
            row(HarnessId::ClaudeCode, U1, 2),
            child(row(HarnessId::ClaudeCode, "agent-a1", 1), U1),
        ],
        ..Stores::default()
    };
    let setup = setup(&format!(r#"{{"{U1}":"alice"}}"#), stores);
    let (_, body) = get(&setup, "/api/harnesses/claude-code/sessions", ALICE).await;
    assert_eq!(plane_ids(&body), [U1, "agent-a1"]);
    let (_, body) = get(&setup, "/api/harnesses/claude-code/sessions", None).await;
    assert_eq!(plane_ids(&body), Vec::<String>::new());
    assert_eq!(
        setup.fixture.registry.owner("agent-a1").expect("key"),
        Some(user("alice"))
    );
}

#[tokio::test]
async fn b3_1_plane_rows_are_summaries_with_millisecond_iso_times() {
    let stores = Stores {
        rows: vec![row(HarnessId::ClaudeCode, U1, 1_760_700_000_000)],
        cwd: BTreeMap::from([(U1.to_owned(), "/home/user/proj".to_owned())]),
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (status, body) = get(&setup, "/api/harnesses/claude-code/sessions", None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        String::from_utf8_lossy(&body),
        format!(
            r#"{{"sessions":[{{"sessionId":"claude-code:{U1}","harnessId":"claude-code","cwd":"/home/user/proj","createdAt":"2025-10-17T11:20:00.000Z","updatedAt":"2025-10-17T11:20:00.000Z","status":"ended"}}]}}"#
        )
    );
    assert_eq!(*setup.lister.limits.lock(), [100]);
}

#[tokio::test]
async fn b3_2_a_title_equal_to_the_id_is_omitted() {
    let stores = Stores {
        rows: vec![
            row(HarnessId::ClaudeCode, U1, 2),
            titled(row(HarnessId::ClaudeCode, U2, 1), "named"),
        ],
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (_, body) = get(&setup, "/api/harnesses/claude-code/sessions?limit=5", None).await;
    let sessions = parsed::<HarnessSessionList>(&body).sessions;
    assert_eq!(sessions.len(), 2);
    assert_eq!(nth(&sessions, 0).title, None);
    assert_eq!(nth(&sessions, 1).title.as_deref(), Some("named"));
    assert_eq!(*setup.lister.limits.lock(), [100]);
}

#[tokio::test]
async fn b3_3_an_unknown_harness_is_not_found() {
    let setup = setup("{}", Stores::default());
    let (status, body) = get(&setup, "/api/harnesses/claude/sessions", None).await;
    assert_eq!(status, 404);
    assert_eq!(error(&body).error, "unknown harness: claude");
}

#[tokio::test]
async fn b3_5_a_subagent_row_links_its_canonical_parent() {
    let stores = Stores {
        rows: vec![
            row(HarnessId::ClaudeCode, U1, 2),
            child(row(HarnessId::ClaudeCode, "agent-a1", 1), U1),
        ],
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (_, body) = get(&setup, "/api/harnesses/claude-code/sessions", None).await;
    let sessions = parsed::<HarnessSessionList>(&body).sessions;
    assert_eq!(nth(&sessions, 1).parent_session_id, Some(claude(U1)));
}

fn one_claude_row() -> Stores {
    Stores {
        rows: vec![titled(
            row(HarnessId::ClaudeCode, U1, 1_760_700_000_000),
            "fix it",
        )],
        ..Stores::default()
    }
}

#[tokio::test]
async fn b4_1_and_t0_9_a_bare_uuid_resolves_to_its_store_and_redirects() {
    let setup = setup("{}", one_claude_row());
    for _ in 0..2 {
        let (status, body) = get(&setup, &format!("/api/harness-sessions/{U1}"), None).await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let response: HarnessSessionResponse = parsed(&body);
        assert_eq!(response.summary.session_id, claude(U1));
        assert_eq!(response.redirected_to, Some(claude(U1)));
        assert_eq!(response.summary.title.as_deref(), Some("fix it"));
    }
    let (status, body) = get(&setup, &format!("/api/harness-sessions/{U2}"), None).await;
    assert_eq!(status, 400);
    let refused = error(&body);
    assert_eq!(refused.code.as_deref(), Some("invalid_session_id"));
    assert_eq!(refused.retryable, Some(false));
    assert_eq!(
        refused.error,
        format!("no registered harness owns bare session id {U2}")
    );
}

#[tokio::test]
async fn t0_6_a_path_fallback_id_redirects_to_the_collapsed_id() {
    let setup = setup("{}", one_claude_row());
    let segment = enc(&format!("claude-code:-home-user/{U1}"));
    let (status, body) = get(&setup, &format!("/api/harness-sessions/{segment}"), None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let response: HarnessSessionResponse = parsed(&body);
    assert_eq!(response.redirected_to, Some(claude(U1)));
    let canonical = enc(&format!("claude-code:{U1}"));
    let (_, body) = get(&setup, &format!("/api/harness-sessions/{canonical}"), None).await;
    assert!(!String::from_utf8_lossy(&body).contains("redirectedTo"));
}

#[tokio::test]
async fn b4_2_an_unknown_session_without_a_live_chat_is_not_found() {
    let setup = setup("{}", one_claude_row());
    let segment = enc(&format!("claude-code:{U2}"));
    let (status, body) = get(&setup, &format!("/api/harness-sessions/{segment}"), None).await;
    assert_eq!(status, 404);
    assert_eq!(
        error(&body).error,
        format!("unknown session claude-code:{U2}")
    );
}

#[tokio::test]
async fn b4_3_another_users_session_is_forbidden() {
    let setup = setup(
        &format!(r#"{{"claude-code:{U1}":"owner"}}"#),
        one_claude_row(),
    );
    let segment = enc(&format!("claude-code:{U1}"));
    let (status, body) = get(&setup, &format!("/api/harness-sessions/{segment}"), ALICE).await;
    assert_eq!(status, 403);
    assert_eq!(error(&body).error, "session is owned by another user");
    let (status, _) = get(
        &setup,
        &format!("/api/harness-sessions/{segment}/transcript"),
        ALICE,
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn segments_that_do_not_decode_are_bad_requests() {
    let setup = setup("{}", one_claude_row());
    for segment in ["abc=", "IGNsYXVkZS1jb2RlOng", "task%3A123"] {
        let (status, body) = get(&setup, &format!("/api/harness-sessions/{segment}"), None).await;
        assert_eq!(status, 400, "{segment}");
        assert_eq!(error(&body).code.as_deref(), Some("invalid_session_id"));
    }
    let segment = enc(&format!("claude-code:{U1}"));
    let (status, body) = http(
        &setup.fixture.gateway,
        "DELETE",
        &format!("/api/harness-sessions/{segment}"),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 405);
    assert_eq!(error(&body).error, "method not allowed");
}

#[tokio::test]
async fn b5_the_transcript_is_store_scoped_and_carries_the_context_stamp() {
    let stores = Stores {
        rows: vec![
            row(HarnessId::ClaudeCode, U1, 1),
            row(HarnessId::Hermes, "h1", 1),
        ],
        transcripts: BTreeMap::from([
            (
                (HarnessId::ClaudeCode, U1.to_owned()),
                vec![
                    turn(SessionMessageRole::User, "hi"),
                    answered("hello", "claude-opus-4-1[1m]", 10),
                ],
            ),
            (
                (HarnessId::ClaudeCode, "h1".to_owned()),
                vec![turn(SessionMessageRole::User, "claude, not hermes")],
            ),
        ]),
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let segment = enc(&format!("claude-code:{U1}"));
    let (status, body) = get(
        &setup,
        &format!("/api/harness-sessions/{segment}/transcript"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let read: HarnessSessionTranscriptResponse = parsed(&body);
    assert_eq!(read.session_id, claude(U1));
    assert_eq!(read.turns.len(), 2);
    assert_eq!(read.context_window, 1_000_000);
    assert_eq!(read.compact_at, 965_000);
    assert_eq!(read.context_source, ContextSource::Default);
    let (status, body) = get(
        &setup,
        &format!("/api/harness-sessions/{}/transcript", enc("hermes:h1")),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let hermes: HarnessSessionTranscriptResponse = parsed(&body);
    assert_eq!(hermes.turns, Vec::new());
    assert_eq!(hermes.context_window, 262_144);
}

#[tokio::test]
async fn observed_usage_promotes_the_window_and_learns_the_compact_point() {
    let stores = Stores {
        rows: vec![row(HarnessId::Codex, U1, 1)],
        transcripts: BTreeMap::from([(
            (HarnessId::Codex, U1.to_owned()),
            vec![
                answered("a", "gpt-5", 300_000),
                answered("b", "gpt-5", 420_000),
                answered("c", "gpt-5", 20_000),
            ],
        )]),
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (_, body) = get(
        &setup,
        &format!(
            "/api/harness-sessions/{}/transcript",
            enc(&format!("codex:{U1}"))
        ),
        None,
    )
    .await;
    let read: HarnessSessionTranscriptResponse = parsed(&body);
    assert_eq!(read.context_window, 1_000_000);
    assert_eq!(read.compact_at, 420_000);
    assert_eq!(read.context_source, ContextSource::Observed);
}

#[tokio::test]
async fn a_prompt_past_a_million_promotes_the_window_to_the_next_million() {
    let stores = Stores {
        rows: vec![row(HarnessId::Codex, U1, 1)],
        transcripts: BTreeMap::from([(
            (HarnessId::Codex, U1.to_owned()),
            vec![answered("a", "gpt-5", 1_085_000)],
        )]),
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (_, body) = get(
        &setup,
        &format!(
            "/api/harness-sessions/{}/transcript",
            enc(&format!("codex:{U1}"))
        ),
        None,
    )
    .await;
    let read: HarnessSessionTranscriptResponse = parsed(&body);
    assert_eq!(read.context_window, 2_000_000);
    assert_eq!(read.context_source, ContextSource::Observed);
}

#[tokio::test]
async fn a_harness_without_a_transcript_reader_is_unsupported() {
    let stores = Stores {
        rows: vec![row(HarnessId::Cowork, "c1", 1)],
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let (status, body) = get(
        &setup,
        &format!("/api/harness-sessions/{}/transcript", enc("cowork:c1")),
        None,
    )
    .await;
    assert_eq!(status, 501);
    let refused = error(&body);
    assert_eq!(refused.code.as_deref(), Some("capability_unsupported"));
    assert_eq!(refused.error, "cowork has no transcript source");
}

fn nth<T>(items: &[T], index: usize) -> &T {
    items.get(index).expect("listed item")
}

#[test]
fn harness_codes_map_to_their_statuses() {
    use strum::IntoEnumIterator as _;
    for code in wire::harness::HarnessCode::iter() {
        assert_eq!(gateway::error::harness_status(code).as_u16(), code.status());
    }
}

fn previewed(body: &[u8]) -> Vec<(SessionId, SessionPreviewKind, String)> {
    parsed::<SessionPreviewList>(body)
        .previews
        .into_iter()
        .map(|preview| (preview.session_id, preview.kind, preview.text))
        .collect()
}

#[tokio::test]
async fn previews_show_the_last_line_of_the_last_reply_and_refuse_bad_requests() {
    let stores = Stores {
        rows: vec![
            row(HarnessId::ClaudeCode, U1, 1),
            row(HarnessId::ClaudeCode, U2, 1),
        ],
        transcripts: BTreeMap::from([(
            (HarnessId::ClaudeCode, U1.to_owned()),
            vec![
                turn(SessionMessageRole::User, "check it"),
                turn(
                    SessionMessageRole::Assistant,
                    "Done.\nAll four checks pass.",
                ),
            ],
        )]),
        ..Stores::default()
    };
    let setup = setup("{}", stores);
    let path =
        format!("/api/harness-sessions/previews?session=claude-code:{U1}&session=claude-code:{U2}");
    let (status, body) = get(&setup, &path, None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        previewed(&body),
        [(
            claude(U1),
            SessionPreviewKind::Said,
            "All four checks pass.".to_owned()
        )]
    );
    let (status, _) = get(
        &setup,
        &format!("/api/harness-sessions/previews?session={U1}"),
        None,
    )
    .await;
    assert_eq!(status, 400, "a bare id is not a canonical session");
    let many: Vec<String> = (0..51)
        .map(|index| format!("session=claude-code:00000000-0000-4000-8000-{index:012}"))
        .collect();
    let (status, _) = get(
        &setup,
        &format!("/api/harness-sessions/previews?{}", many.join("&")),
        None,
    )
    .await;
    assert_eq!(status, 400, "more than 50 sessions is refused");
}

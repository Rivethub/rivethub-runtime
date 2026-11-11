#![cfg(test)]
use crate::session_support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use gateway::sessions::{SessionRegistry, owners_file};
use kernel::memory::SessionActivity;
use kernel::transcript::{
    HarnessTranscriptTool, HarnessTranscriptTurn, SessionMessageRole, TranscriptToolStatus,
};
use kernel::transcript::{
    ListLimit, SessionLister, SessionRenameError, StoreSession, StoreTranscript, ToolOutputRead,
    TranscriptRead, TranscriptReaderError, TranscriptWindow,
};
use session_support::{Fixture, http, user};
use testkit::FakeSessionDeletion;
use wire::gateway::GatewayError;
use wire::gateway::session_delete::{
    SessionDeleteError, SessionDeleteResult, SessionDeleteResults, SessionDeleted,
    SessionTrashList, SessionTrashReason,
};
use wire::harness::{HarnessId, HarnessSessionList};
use wire::session_id::{SessionId, encode_segment};

const ALICE: Option<&str> = Some("alice-test-token");

#[derive(Default)]
pub(crate) struct Stores {
    pub(crate) rows: Vec<StoreSession>,
    pub(crate) hidden: Vec<StoreSession>,
    pub(crate) transcripts: BTreeMap<String, Vec<HarnessTranscriptTurn>>,
    pub(crate) unreadable: BTreeSet<String>,
}

#[async_trait]
impl SessionLister for Stores {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
        let mut rows: Vec<StoreSession> = self
            .rows
            .iter()
            .filter(|row| harnesses.contains(&row.harness))
            .cloned()
            .collect();
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
            .chain(&self.hidden)
            .find(|row| row.harness == harness && row.id == native)
            .cloned())
    }

    async fn store_cwd(
        &self,
        _harness: HarnessId,
        _native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(None)
    }

    async fn transcript(
        &self,
        _harness: HarnessId,
        native: &str,
        _window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        if self.unreadable.contains(native) {
            return Err(TranscriptReaderError::Read {
                message: "truncated line".to_owned(),
                source: "truncated line".into(),
            });
        }
        Ok(TranscriptRead::Read(StoreTranscript {
            turns: self.transcripts.get(native).cloned().unwrap_or_default(),
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

struct Setup {
    fixture: Fixture,
    deletion: Arc<FakeSessionDeletion>,
    _state: tempfile::TempDir,
}

fn setup(owners: &str, stores: Stores, deletion: FakeSessionDeletion) -> Setup {
    let state = tempfile::tempdir().expect("state");
    let path = owners_file(state.path());
    std::fs::create_dir_all(path.parent().expect("owners directory")).expect("owners directory");
    std::fs::write(&path, owners).expect("owners");
    let registry = SessionRegistry::default()
        .with_owners_file(path)
        .with_node_owner(user("owner"));
    let deletion = Arc::new(deletion);
    let fixture = Fixture::with_deletion(registry, Some(Arc::new(stores)), deletion.clone());
    Setup {
        fixture,
        deletion,
        _state: state,
    }
}

async fn call(
    setup: &Setup,
    method: &str,
    path: &str,
    body: &str,
    token: Option<&str>,
) -> (u16, Vec<u8>) {
    http(
        &setup.fixture.gateway,
        method,
        path,
        body.as_bytes().to_vec(),
        token,
    )
    .await
}

fn parsed<T: serde::de::DeserializeOwned>(body: &[u8]) -> T {
    serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(body)))
}

fn claude(native: &str) -> SessionId {
    SessionId::new(HarnessId::ClaudeCode, native)
}

fn hours_ago(hours: i64) -> i64 {
    (Utc::now() - TimeDelta::hours(hours)).timestamp_millis()
}

fn minutes_ago(minutes: i64) -> i64 {
    (Utc::now() - TimeDelta::minutes(minutes)).timestamp_millis()
}

fn row(native: &str, updated_at: i64) -> StoreSession {
    StoreSession::new(HarnessId::ClaudeCode, native, updated_at)
}

fn titled(native: &str, title: &str) -> StoreSession {
    let mut row = row(native, hours_ago(3));
    title.clone_into(&mut row.title);
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

fn exchange(prompt: &str, reply: &str) -> Vec<HarnessTranscriptTurn> {
    vec![
        turn(SessionMessageRole::User, prompt),
        turn(SessionMessageRole::Assistant, reply),
    ]
}

fn remembered(user_messages: u64, reply: Option<&str>) -> SessionActivity {
    let assistant_messages = u64::from(reply.is_some());
    SessionActivity {
        conversations: 1,
        messages: user_messages + assistant_messages,
        user_messages,
        assistant_messages,
        reply: reply.map(str::to_owned),
        ..SessionActivity::default()
    }
}

fn segment(native: &str) -> String {
    encode_segment(&claude(native))
}

fn listed_ids(body: &[u8]) -> Vec<String> {
    parsed::<HarnessSessionList>(body)
        .sessions
        .into_iter()
        .map(|row| row.session_id.native().to_owned())
        .collect()
}

#[tokio::test]
async fn deleting_a_session_tombstones_it_and_hides_it_from_both_lists() {
    let stores = Stores {
        rows: vec![row("s-keep", hours_ago(2)), row("s-gone", hours_ago(2))],
        ..Stores::default()
    };
    let deletion =
        FakeSessionDeletion::default().with_activity(claude("s-gone"), remembered(2, Some("ok")));
    let setup = setup("{}", stores, deletion);
    let (status, body) = call(
        &setup,
        "DELETE",
        &format!("/api/sessions/{}", segment("s-gone")),
        "",
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        String::from_utf8_lossy(&body),
        r#"{"ok":true,"session":"claude-code:s-gone","conversations":1,"messages":3,"summaries":0}"#
    );
    assert_eq!(
        setup.deletion.deleted_by(claude("s-gone")),
        Some(Some("owner".to_owned()))
    );
    let (_, body) = call(
        &setup,
        "GET",
        "/api/harnesses/claude-code/sessions",
        "",
        None,
    )
    .await;
    assert_eq!(listed_ids(&body), ["s-keep"]);
    let (_, body) = call(
        &setup,
        "GET",
        "/api/harnesses/claude-code/sessions",
        "",
        None,
    )
    .await;
    let sessions: Vec<SessionId> = parsed::<HarnessSessionList>(&body)
        .sessions
        .into_iter()
        .map(|summary| summary.session_id)
        .collect();
    assert_eq!(sessions, [claude("s-keep")]);
}

#[tokio::test]
async fn a_percent_encoded_canonical_id_is_accepted() {
    let stores = Stores {
        rows: vec![row("s-plain", hours_ago(2))],
        ..Stores::default()
    };
    let setup = setup("{}", stores, FakeSessionDeletion::default());
    let (status, body) = call(
        &setup,
        "DELETE",
        "/api/sessions/claude-code%3As-plain",
        "",
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        parsed::<SessionDeleted>(&body).session,
        claude("s-plain").into()
    );
}

#[tokio::test]
async fn a_session_the_user_cannot_see_or_that_is_unknown_is_not_found() {
    let stores = Stores {
        rows: vec![row("s-owned", hours_ago(2))],
        ..Stores::default()
    };
    let setup = setup(
        r#"{"s-owned":"owner"}"#,
        stores,
        FakeSessionDeletion::default(),
    );
    let (status, body) = call(
        &setup,
        "DELETE",
        &format!("/api/sessions/{}", segment("s-owned")),
        "",
        ALICE,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(parsed::<GatewayError>(&body).error, "not found");
    assert_eq!(setup.deletion.deleted_by(claude("s-owned")), None);
    let (status, _) = call(
        &setup,
        "DELETE",
        &format!("/api/sessions/{}", segment("s-nowhere")),
        "",
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(setup.deletion.deleted_by(claude("s-nowhere")), None);
}

#[tokio::test]
async fn the_bulk_endpoint_answers_per_session() {
    let stores = Stores {
        rows: vec![row("s-mine", hours_ago(2)), row("s-theirs", hours_ago(2))],
        ..Stores::default()
    };
    let setup = setup(
        r#"{"s-mine":"alice","s-theirs":"owner"}"#,
        stores,
        FakeSessionDeletion::default(),
    );
    let (status, body) = call(
        &setup,
        "POST",
        "/api/sessions/delete",
        r#"{"sessions":["claude-code:s-mine","claude-code:s-theirs","claude-code:s-mine"]}"#,
        ALICE,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        String::from_utf8_lossy(&body),
        concat!(
            r#"{"results":[{"ok":true,"session":"claude-code:s-mine","conversations":0,"messages":0,"summaries":0},"#,
            r#"{"session":"claude-code:s-theirs","ok":false,"error":"not found"}]}"#
        )
    );
    let results = parsed::<SessionDeleteResults>(&body).results;
    assert!(matches!(
        results.get(1),
        Some(SessionDeleteResult::Refused(refused)) if refused.error == SessionDeleteError::NotFound
    ));
    let (status, _) = call(
        &setup,
        "POST",
        "/api/sessions/delete",
        r#"{"sessions":[]}"#,
        ALICE,
    )
    .await;
    assert_eq!(status, 400);
}

fn tool_turn() -> HarnessTranscriptTurn {
    HarnessTranscriptTurn {
        tools: Some(vec![HarnessTranscriptTool {
            name: "Bash".to_owned(),
            status: TranscriptToolStatus::Done,
            args: None,
            id: Some("t1".to_owned()),
            input: None,
            result_text: None,
        }]),
        ..turn(SessionMessageRole::Assistant, "Done.")
    }
}

fn junk_stores() -> Stores {
    let mut transcripts = BTreeMap::new();
    transcripts.insert("t-hi".to_owned(), exchange("hi", "Hello!"));
    transcripts.insert("t-two-words".to_owned(), exchange("hi", "Hello there"));
    transcripts.insert(
        "t-tool".to_owned(),
        vec![turn(SessionMessageRole::User, "ls"), tool_turn()],
    );
    transcripts.insert("t-placed".to_owned(), exchange("hi", "Hello!"));
    transcripts.insert("t-cited".to_owned(), exchange("hi", "Hello!"));
    transcripts.insert("t-recent".to_owned(), exchange("hi", "Hello!"));
    Stores {
        rows: vec![
            row("t-empty", hours_ago(3)),
            titled("t-untitled", "Untitled"),
            row("t-hi", hours_ago(3)),
            row("t-two-words", hours_ago(3)),
            row("t-tool", hours_ago(3)),
            row("t-placed", hours_ago(3)),
            row("t-cited", hours_ago(3)),
            row("t-recent", minutes_ago(20)),
            row("t-unreadable", hours_ago(3)),
            titled("t-named", "Release checklist"),
        ],
        transcripts,
        unreadable: BTreeSet::from(["t-unreadable".to_owned()]),
        ..Stores::default()
    }
}

fn junk_memory() -> FakeSessionDeletion {
    FakeSessionDeletion::default()
        .with_activity(claude("t-hi"), remembered(1, Some("Hello!")))
        .with_activity(
            claude("t-placed"),
            SessionActivity {
                placed: true,
                ..remembered(1, Some("Hello!"))
            },
        )
        .with_activity(
            claude("t-cited"),
            SessionActivity {
                cited: true,
                ..remembered(1, Some("Hello!"))
            },
        )
}

#[tokio::test]
async fn trash_lists_only_obvious_junk() {
    let setup = setup("{}", junk_stores(), junk_memory());
    let (status, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let candidates = parsed::<SessionTrashList>(&body).candidates;
    let found: Vec<(String, SessionTrashReason, Option<String>)> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.session.native().to_owned(),
                candidate.reason,
                candidate.reply.clone(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("t-empty".to_owned(), SessionTrashReason::Empty, None),
            ("t-untitled".to_owned(), SessionTrashReason::Empty, None),
            (
                "t-hi".to_owned(),
                SessionTrashReason::OneWordReply,
                Some("Hello!".to_owned())
            ),
        ]
    );
    let first = candidates.first().expect("a candidate");
    assert_eq!(first.title, None);
    assert_eq!(first.user_turns, 0);
    assert_eq!(
        candidates.get(1).and_then(|c| c.title.clone()).as_deref(),
        Some("Untitled")
    );
    let json = String::from_utf8_lossy(&body);
    assert!(
        json.contains(r#""reason":"one-word-reply","userTurns":1,"reply":"Hello!""#),
        "{json}"
    );
    assert!(json.contains(r#""title":null"#), "{json}");
    assert_eq!(
        setup.deletion.activity_of(&claude("t-hi")),
        Some(remembered(1, Some("Hello!")))
    );
}

#[tokio::test]
async fn trash_lists_a_remembered_session_the_list_hides_until_it_is_deleted() {
    let stores = Stores {
        hidden: vec![
            row("t-discovery", hours_ago(3)),
            row("t-hidden-elsewhere", hours_ago(3)),
        ],
        ..Stores::default()
    };
    let deletion = FakeSessionDeletion::default().with_activity(
        claude("t-discovery"),
        SessionActivity {
            conversations: 1,
            ..SessionActivity::default()
        },
    );
    let setup = setup("{}", stores, deletion);
    let (status, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let found: Vec<(String, SessionTrashReason)> = parsed::<SessionTrashList>(&body)
        .candidates
        .into_iter()
        .map(|candidate| (candidate.session.native().to_owned(), candidate.reason))
        .collect();
    assert_eq!(
        found,
        [("t-discovery".to_owned(), SessionTrashReason::Empty)]
    );
    let path = format!("/api/sessions/{}", segment("t-discovery"));
    let (status, _) = call(&setup, "DELETE", &path, "", None).await;
    assert_eq!(status, 200);
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(parsed::<SessionTrashList>(&body).candidates, Vec::new());
}

#[tokio::test]
async fn trash_skips_tombstoned_live_and_unseen_sessions() {
    let stores = Stores {
        rows: vec![row("t-gone", hours_ago(3)), row("t-alice", hours_ago(3))],
        ..Stores::default()
    };
    let deletion = FakeSessionDeletion::default();
    deletion.tombstone(&claude("t-gone"), Utc::now());
    let setup = setup(r#"{"t-alice":"alice"}"#, stores, deletion);
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(String::from_utf8_lossy(&body), r#"{"candidates":[]}"#);
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", ALICE).await;
    let found: Vec<SessionId> = parsed::<SessionTrashList>(&body)
        .candidates
        .into_iter()
        .map(|candidate| candidate.session)
        .collect();
    assert_eq!(found, [claude("t-alice")]);
}

#[tokio::test]
async fn memory_tool_rows_keep_a_session_out_of_the_trash() {
    let stores = Stores {
        rows: vec![row("t-mem-tool", hours_ago(3))],
        ..Stores::default()
    };
    let deletion = FakeSessionDeletion::default().with_activity(
        claude("t-mem-tool"),
        SessionActivity {
            tool_rows: 1,
            ..remembered(1, None)
        },
    );
    let setup = setup("{}", stores, deletion);
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(String::from_utf8_lossy(&body), r#"{"candidates":[]}"#);
}

#[tokio::test]
async fn a_deleted_session_resumed_in_its_harness_is_listed_again() {
    let stores = Stores {
        rows: vec![
            row("s-resumed", minutes_ago(1)),
            row("s-gone", hours_ago(2)),
        ],
        ..Stores::default()
    };
    let deletion = FakeSessionDeletion::default();
    let deleted_at = Utc::now() - TimeDelta::hours(1);
    deletion.tombstone(&claude("s-resumed"), deleted_at);
    deletion.tombstone(&claude("s-gone"), deleted_at);
    let setup = setup("{}", stores, deletion);
    let (_, body) = call(
        &setup,
        "GET",
        "/api/harnesses/claude-code/sessions",
        "",
        None,
    )
    .await;
    assert_eq!(listed_ids(&body), ["s-resumed"]);
    let (_, body) = call(
        &setup,
        "GET",
        "/api/harnesses/claude-code/sessions",
        "",
        None,
    )
    .await;
    let sessions: Vec<SessionId> = parsed::<HarnessSessionList>(&body)
        .sessions
        .into_iter()
        .map(|summary| summary.session_id)
        .collect();
    assert_eq!(sessions, [claude("s-resumed")]);
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(String::from_utf8_lossy(&body), r#"{"candidates":[]}"#);
}

#[tokio::test]
async fn a_kept_session_leaves_the_trash_until_it_is_used_again() {
    let stores = Stores {
        rows: vec![
            row("t-kept", hours_ago(3)),
            row("t-used-since", hours_ago(2)),
        ],
        ..Stores::default()
    };
    let deletion = FakeSessionDeletion::default();
    deletion.keep(&claude("t-used-since"), Utc::now() - TimeDelta::hours(3));
    let setup = setup("{}", stores, deletion);
    let trash = |body: &[u8]| -> Vec<String> {
        parsed::<SessionTrashList>(body)
            .candidates
            .into_iter()
            .map(|candidate| candidate.session.native().to_owned())
            .collect()
    };
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(trash(&body), ["t-kept", "t-used-since"]);
    let path = format!("/api/sessions/{}/keep", segment("t-kept"));
    let (status, body) = call(&setup, "PUT", &path, "", None).await;
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    let (_, body) = call(&setup, "GET", "/api/sessions/trash", "", None).await;
    assert_eq!(trash(&body), ["t-used-since"]);
    let unknown = format!("/api/sessions/{}/keep", segment("t-unknown"));
    let (status, _) = call(&setup, "PUT", &unknown, "", None).await;
    assert_eq!(status, 404);
}

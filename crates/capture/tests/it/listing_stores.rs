use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use capture::listing::with_ancestors;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::StoreSession;
use rusqlite::Connection;
use wire::harness::HarnessId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const U1: &str = "00000000-0000-4000-8000-000000000001";
const U2: &str = "00000000-0000-4000-8000-000000000002";

fn write(root: &Path, relative: &str, content: &str) -> Result<PathBuf, std::io::Error> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(path)
}

fn set_mtime(path: &Path, ms: u64) -> Result<(), std::io::Error> {
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
}

fn lister(roots: StoreRoots) -> StoreSessionLister {
    StoreSessionLister::new(roots)
}

async fn only(lister: &StoreSessionLister, harness: HarnessId) -> Vec<StoreSession> {
    lister.list(&[harness], ListLimit::DEFAULT).await
}

fn summary(row: &StoreSession) -> (&str, &str, i64) {
    (row.id.as_str(), row.title.as_str(), row.updated_at)
}

fn hermes_db(root: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    fs::create_dir_all(root)?;
    let connection = Connection::open(root.join("state.db"))?;
    connection.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, started_at, ended_at);\
         CREATE TABLE messages (session_id TEXT, role TEXT, content TEXT, timestamp);",
    )?;
    Ok(connection)
}

#[tokio::test]
async fn hermes_lists_the_sessions_table_with_the_first_user_message() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = hermes_db(dir.path())?;
    connection.execute_batch(
        "INSERT INTO sessions VALUES ('h1', 1700000000, NULL);\
         INSERT INTO sessions VALUES ('h2', 1700000000123, NULL);\
         INSERT INTO sessions VALUES ('h3', 1600000000, 1600000500);\
         INSERT INTO messages VALUES ('h1', 'assistant', 'not me', 1);\
         INSERT INTO messages VALUES ('h1', 'user', '  hi ', 2);\
         INSERT INTO messages VALUES ('h1', 'user', 'later', 3);\
         INSERT INTO messages VALUES ('h2', 'user', '', 1);\
         INSERT INTO messages VALUES ('h3', 'user', 'old', 1);",
    )?;
    let lister = lister(StoreRoots {
        hermes: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Hermes).await;
    let brief: Vec<_> = rows.iter().map(summary).collect();
    assert_eq!(
        brief,
        vec![
            ("h2", "h2", 1_700_000_000_123),
            ("h1", "hi", 1_700_000_000_000),
            ("h3", "old", 1_600_000_500_000),
        ]
    );
    assert!(rows.iter().all(|row| row.created_at.is_none()));
    let described = lister
        .describe(HarnessId::Hermes, "h3", None)
        .await?
        .ok_or("h3")?;
    assert_eq!(described.created_at, Some(1_600_000_000_000));
    Ok(())
}

#[tokio::test]
async fn hermes_lists_no_unprompted_session_and_missing_db_is_empty() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = hermes_db(&dir.path().join("present"))?;
    connection.execute("INSERT INTO sessions VALUES ('h1', 1700000000, NULL)", [])?;
    let present = lister(StoreRoots {
        hermes: Some(dir.path().join("present")),
        ..StoreRoots::default()
    });
    assert_eq!(
        only(&present, HarnessId::Hermes).await,
        Vec::<StoreSession>::new()
    );
    let described = present
        .describe(HarnessId::Hermes, "h1", None)
        .await?
        .ok_or("h1")?;
    assert_eq!(described.title, "h1");
    let missing = lister(StoreRoots {
        hermes: Some(dir.path().join("missing")),
        ..StoreRoots::default()
    });
    assert_eq!(
        only(&missing, HarnessId::Hermes).await,
        Vec::<StoreSession>::new()
    );
    Ok(())
}

fn opencode_db(root: &Path, with_model: bool) -> Result<Connection, Box<dyn std::error::Error>> {
    fs::create_dir_all(root)?;
    let connection = Connection::open(root.join("opencode.db"))?;
    let model = if with_model { "model TEXT," } else { "" };
    connection.execute_batch(&format!(
        "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, {model} time_created, time_updated);\
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT);"
    ))?;
    Ok(connection)
}

#[tokio::test]
async fn opencode_reads_the_session_table_and_labels_models() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = opencode_db(dir.path(), true)?;
    connection.execute_batch(
        "INSERT INTO session VALUES ('ses_a', 'T  x', '{\"providerID\":\"anthropic\",\"id\":\"claude\"}', 1690000000, 1700000000);\
         INSERT INTO session VALUES ('ses_b', '', 'gpt-5', 1600000000000, 0);\
         INSERT INTO session VALUES ('ses_c', NULL, '{\"id\":\"only-id\"}', NULL, 1500000000);\
         INSERT INTO session VALUES ('ses_d', 'New session - 2026-10-28T00:00:00.000Z', NULL, 1800000000, 1800000000);\
         INSERT INTO message VALUES ('m_a', 'ses_a');\
         INSERT INTO message VALUES ('m_b', 'ses_b');\
         INSERT INTO message VALUES ('m_c', 'ses_c');",
    )?;
    let lister = lister(StoreRoots {
        opencode: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Opencode).await;
    let brief: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.title.as_str(),
                row.updated_at,
                row.model.as_deref(),
                row.created_at,
            )
        })
        .collect();
    assert_eq!(
        brief,
        vec![
            (
                "ses_a",
                "T x",
                1_700_000_000_000,
                Some("anthropic/claude"),
                Some(1_690_000_000_000)
            ),
            (
                "ses_b",
                "ses_b",
                1_600_000_000_000,
                Some("gpt-5"),
                Some(1_600_000_000_000)
            ),
            ("ses_c", "ses_c", 1_500_000_000_000, Some("only-id"), None),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn opencode_without_a_model_column_still_lists_rows() -> TestResult {
    let dir = tempfile::tempdir()?;
    let connection = opencode_db(dir.path(), false)?;
    connection.execute_batch(
        "INSERT INTO session VALUES ('ses_a', 'T', 1690000000, 1700000000);\
         INSERT INTO message VALUES ('m_a', 'ses_a');",
    )?;
    let lister = lister(StoreRoots {
        opencode: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Opencode).await;
    assert_eq!(
        rows.iter().map(summary).collect::<Vec<_>>(),
        vec![("ses_a", "T", 1_700_000_000_000)]
    );
    assert_eq!(rows.first().and_then(|row| row.model.clone()), None);
    let described = lister
        .describe(HarnessId::Opencode, "ses_a", None)
        .await?
        .ok_or("ses_a")?;
    assert_eq!(described.title, "T");
    Ok(())
}

fn kimi_lister(root: &Path) -> StoreSessionLister {
    lister(StoreRoots {
        kimi: Some(root.to_path_buf()),
        ..StoreRoots::default()
    })
}

fn kimi_append(kind: &str, content: &str) -> String {
    format!(
        r#"{{"type":"context.append_message","message":{{"role":"user","origin":{{"kind":"{kind}"}},"content":{content}}}}}"#
    )
}

#[tokio::test]
async fn kimi_keeps_the_session_prefix_and_skips_injected_turns() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        "sessions/wd_x/session_ab12/state.json",
        r#"{"updatedAt":1760000000000}"#,
    )?;
    write(
        dir.path(),
        "sessions/wd_x/session_ab12/agents/main/wire.jsonl",
        &[
            kimi_append("injection", r#""banner""#),
            kimi_append("user", r#"[{"type":"text","text":"real ask"}]"#),
        ]
        .join("\n"),
    )?;
    let rows = only(&kimi_lister(dir.path()), HarnessId::KimiCode).await;
    assert_eq!(
        rows.iter().map(summary).collect::<Vec<_>>(),
        vec![("session_ab12", "real ask", 1_760_000_000_000)]
    );
    Ok(())
}

#[tokio::test]
async fn kimi_state_title_collapses_and_times_fall_back_to_mtime() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        "sessions/wd_x/session_a/state.json",
        r#"{"title":"a\n\n b","updatedAt":"2026-10-01T00:00:00Z","createdAt":"2026-09-30"}"#,
    )?;
    let state = write(
        dir.path(),
        "sessions/wd_x/session_b/state.json",
        r#"{"lastPrompt":"  prompt "}"#,
    )?;
    set_mtime(&state, 1_000_000_000_000)?;
    write(dir.path(), "sessions/wd_x/session_c/state.json", "broken")?;
    write(dir.path(), "sessions/wd_x/other/state.json", "{}")?;
    let rows = only(&kimi_lister(dir.path()), HarnessId::KimiCode).await;
    let mut brief: Vec<_> = rows
        .iter()
        .map(|row| (summary(row), row.created_at))
        .collect();
    brief.sort_unstable();
    assert_eq!(
        brief,
        vec![
            (
                ("session_a", "a b", 1_790_812_800_000),
                Some(1_790_726_400_000)
            ),
            (("session_b", "prompt", 1_000_000_000_000), None),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn kimi_wire_title_skips_system_reminders() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        "sessions/wd_x/session_a/state.json",
        r#"{"updatedAt":5}"#,
    )?;
    write(
        dir.path(),
        "sessions/wd_x/session_a/agents/main/wire.jsonl",
        &[
            kimi_append("user", r#""<system-reminder>be nice</system-reminder>""#),
            kimi_append("user", r#""<user_query> the human turn </user_query>""#),
        ]
        .join("\n"),
    )?;
    let lister = kimi_lister(dir.path());
    let rows = only(&lister, HarnessId::KimiCode).await;
    assert_eq!(
        rows.first().map(|row| row.title.as_str()),
        Some("the human turn")
    );
    let described = lister
        .describe(HarnessId::KimiCode, "session_a", None)
        .await?
        .ok_or("described")?;
    assert_eq!(described.title, "the human turn");
    Ok(())
}

fn codex_line(text: &str) -> String {
    format!(
        r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
    )
}

#[tokio::test]
async fn codex_titles_skip_environment_context_and_ignore_bad_date_dirs() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("sessions/2026/10/01/rollout-2026-10-01T00-00-00-{U1}.jsonl"),
        &[
            r#"{"type":"session_meta","payload":{}}"#.to_owned(),
            codex_line("<environment_context>cwd</environment_context>"),
            codex_line("do   x"),
        ]
        .join("\n"),
    )?;
    write(
        dir.path(),
        &format!("sessions/2026/1/01/rollout-2026-10-01T00-00-00-{U2}.jsonl"),
        &codex_line("hidden"),
    )?;
    let lister = lister(StoreRoots {
        codex: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Codex).await;
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_str(), row.title.as_str()))
            .collect::<Vec<_>>(),
        vec![(U1, "do x")]
    );
    assert!(lister.describe(HarnessId::Codex, U1, None).await?.is_some());
    assert_eq!(lister.describe(HarnessId::Codex, U2, None).await?, None);
    Ok(())
}

#[tokio::test]
async fn a_codex_title_skips_the_agents_instructions_block() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("sessions/2026/10/01/rollout-2026-10-01T00-00-00-{U1}.jsonl"),
        &[
            r#"{"type":"session_meta","payload":{}}"#.to_owned(),
            codex_line(r"# AGENTS.md instructions for /home/user/app\n\n<INSTRUCTIONS>\nrules\n</INSTRUCTIONS>"),
            codex_line("fix the flaky cache test"),
        ]
        .join("\n"),
    )?;
    let lister = lister(StoreRoots {
        codex: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Codex).await;
    assert_eq!(
        rows.first().map(|row| row.title.as_str()),
        Some("fix the flaky cache test")
    );
    Ok(())
}

fn omp_lister(root: &Path) -> StoreSessionLister {
    lister(StoreRoots {
        omp: Some(root.to_path_buf()),
        ..StoreRoots::default()
    })
}

fn omp_user(text: &str) -> String {
    format!(
        r#"{{"type":"message","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn omp_title(title: &str) -> String {
    format!(r#"{{"type":"title","v":1,"title":"{title}","pad":"  "}}"#)
}

#[tokio::test]
async fn omp_lists_prompted_flat_files_and_keeps_the_newest_copy() -> TestResult {
    let dir = tempfile::tempdir()?;
    let flat = write(
        dir.path(),
        &format!("sessions/2026-10-01T00-00-00_{U1}.jsonl"),
        &omp_user("flat"),
    )?;
    let bucketed = write(
        dir.path(),
        &format!("sessions/-work/2026-09-01T00-00-00_{U1}.jsonl"),
        &omp_user("bucketed"),
    )?;
    write(
        dir.path(),
        &format!("sessions/.hidden/x_{U2}.jsonl"),
        &omp_user("hidden"),
    )?;
    write(
        dir.path(),
        &format!("sessions/-.rivethub-discovery/p_{U2}.jsonl"),
        &[
            omp_title(""),
            r#"{"type":"model_change","model":"acme/probe"}"#.to_owned(),
        ]
        .join("\n"),
    )?;
    set_mtime(&flat, 2_000_000)?;
    set_mtime(&bucketed, 1_000_000)?;
    let lister = omp_lister(dir.path());
    let rows = only(&lister, HarnessId::Omp).await;
    assert_eq!(
        rows.iter().map(summary).collect::<Vec<_>>(),
        vec![(U1, "flat", 2_000_000)]
    );
    assert!(lister.describe(HarnessId::Omp, U2, None).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn omp_title_line_wins_over_the_user_message_and_the_last_model_is_kept() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("sessions/-work/p_{U1}.jsonl"),
        &[
            omp_title("Named"),
            r#"{"type":"model_change","model":"acme/first"}"#.to_owned(),
            omp_user("<command-x>wrapped"),
            r#"{"type":"model_change","model":"acme/second"}"#.to_owned(),
        ]
        .join("\n"),
    )?;
    write(
        dir.path(),
        &format!("sessions/-work/p_{U2}.jsonl"),
        &[omp_title(" "), omp_user("<command-x>wrapped")].join("\n"),
    )?;
    let lister = omp_lister(dir.path());
    let rows = only(&lister, HarnessId::Omp).await;
    let mut brief: Vec<_> = rows
        .iter()
        .map(|row| (row.id.as_str(), row.title.as_str(), row.model.as_deref()))
        .collect();
    brief.sort_unstable();
    assert_eq!(
        brief,
        vec![
            (U1, "Named", Some("acme/second")),
            (U2, "<command-x>wrapped", None)
        ]
    );
    Ok(())
}

#[tokio::test]
async fn omp_reader_reads_a_refused_reply_as_its_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let said = |message: &str| {
        format!(
            r#"{{"type":"message","timestamp":"2026-10-29T06:31:22.000Z","message":{message}}}"#
        )
    };
    write(
        dir.path(),
        &format!("sessions/-work/p_{U1}.jsonl"),
        &[
            format!(r#"{{"type":"session","id":"{U1}","timestamp":"2026-10-29T06:31:21.000Z"}}"#),
            said(r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#),
            said(r#"{"role":"assistant","content":[],"stopReason":"error","errorMessage":"403 out of credits"}"#),
            said(r#"{"role":"assistant","content":[{"type":"text","text":"half"}],"stopReason":"error","errorMessage":"reset"}"#),
        ]
        .join("\n"),
    )?;
    let reader = capture::readers::StoreReader::open(HarnessId::Omp, dir.path().to_path_buf())
        .ok_or("reader")?;
    let turns = kernel::transcript::TranscriptReader::read(
        &reader,
        &wire::session_id::SessionId::new(HarnessId::Omp, U1),
    )
    .await?;
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.content.as_str())
            .collect::<Vec<_>>(),
        ["hi", "403 out of credits", "half\n\nreset"]
    );
    Ok(())
}

#[tokio::test]
async fn omp_lists_a_session_opened_with_a_long_prompt_and_keeps_its_early_model() -> TestResult {
    let dir = tempfile::tempdir()?;
    let long = "x".repeat(70_000);
    write(
        dir.path(),
        &format!("sessions/-work/p_{U1}.jsonl"),
        &[
            omp_title(""),
            r#"{"type":"model_change","model":"acme/early"}"#.to_owned(),
            omp_user(&long),
            omp_user("short"),
        ]
        .join("\n"),
    )?;
    let rows = only(&omp_lister(dir.path()), HarnessId::Omp).await;
    let brief: Vec<_> = rows
        .iter()
        .map(|row| (row.id.as_str(), row.model.as_deref()))
        .collect();
    assert_eq!(brief, vec![(U1, Some("acme/early"))]);
    Ok(())
}

#[tokio::test]
async fn omp_reader_reads_a_flat_session_file() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("sessions/2026-10-01T00-00-00_{U1}.jsonl"),
        &omp_user("flat"),
    )?;
    let reader = capture::readers::StoreReader::open(HarnessId::Omp, dir.path().to_path_buf())
        .ok_or("reader")?;
    let sessions = kernel::transcript::TranscriptReader::sessions(&reader, dir.path()).await?;
    assert_eq!(
        sessions,
        vec![wire::session_id::SessionId::new(HarnessId::Omp, U1)]
    );
    Ok(())
}

#[tokio::test]
async fn cursor_title_collapses_whitespace() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        &format!("projects/slug/agent-transcripts/{U1}/{U1}.jsonl"),
        r#"{"role":"user","message":{"content":[{"type":"text","text":"  hi\nthere "}]}}"#,
    )?;
    write(
        dir.path(),
        "projects/slug/agent-transcripts/not-a-uuid/not-a-uuid.jsonl",
        "{}",
    )?;
    let lister = lister(StoreRoots {
        cursor: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Cursor).await;
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_str(), row.title.as_str()))
            .collect::<Vec<_>>(),
        vec![(U1, "hi there")]
    );
    assert!(rows.iter().all(|row| row.created_at.is_some()));
    Ok(())
}

#[tokio::test]
async fn cowork_lists_archived_tasks_and_skips_invalid_metadata() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        "local-agent-mode-sessions/a/b/local_x.json",
        r#"{"cliSessionId":"c1","sessionId":"local_1","title":"T","createdAt":"1700000000000","lastActivityAt":1700000001000,"archived":true,"cwd":" /home/user/w "}"#,
    )?;
    write(
        dir.path(),
        "claude-code-sessions/local_y.json",
        r#"{"cliSessionId":"c2","title":"no created"}"#,
    )?;
    write(
        dir.path(),
        "claude-code-sessions/local_z.json",
        r#"{"cliSessionId":"../x","createdAt":1}"#,
    )?;
    write(
        dir.path(),
        "claude-code-sessions/1/2/3/4/5/6/7/local_deep.json",
        r#"{"cliSessionId":"deep","createdAt":1}"#,
    )?;
    let lister = lister(StoreRoots {
        cowork: vec![dir.path().to_path_buf(), dir.path().to_path_buf()],
        ..StoreRoots::default()
    });
    let rows = only(&lister, HarnessId::Cowork).await;
    let mut expected = StoreSession::new(HarnessId::Cowork, "c1", 1_700_000_001_000);
    expected.title = "T".to_owned();
    expected.created_at = Some(1_700_000_000_000);
    expected.cwd = Some("/home/user/w".to_owned());
    assert_eq!(rows, vec![expected]);
    assert_eq!(
        lister.store_cwd(HarnessId::Cowork, "c1").await?.as_deref(),
        Some("/home/user/w")
    );
    Ok(())
}

fn row(harness: HarnessId, id: &str, updated_at: i64, parent: Option<&str>) -> StoreSession {
    let mut row = StoreSession::new(harness, id, updated_at);
    row.parent_session_id = parent.map(str::to_owned);
    row
}

fn ids(rows: &[StoreSession]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

#[test]
fn with_ancestors_appends_a_parent_outside_the_cap() {
    let ranked = vec![
        row(HarnessId::ClaudeCode, "c", 10, Some("p")),
        row(HarnessId::ClaudeCode, "x", 9, None),
        row(HarnessId::ClaudeCode, "p", 1, None),
    ];
    assert_eq!(ids(&with_ancestors(ranked, 2)), vec!["c", "x", "p"]);
}

#[test]
fn with_ancestors_stays_inside_one_harness() {
    let ranked = vec![
        row(HarnessId::ClaudeCode, "c", 10, Some("p")),
        row(HarnessId::ClaudeCode, "x", 9, None),
        row(HarnessId::GrokBuild, "p", 1, None),
    ];
    assert_eq!(ids(&with_ancestors(ranked, 2)), vec!["c", "x"]);
}

#[test]
fn with_ancestors_follows_at_most_eight_links() {
    let mut ranked = vec![row(HarnessId::ClaudeCode, "leaf", 100, Some("a1"))];
    for depth in 1..=10 {
        let parent = format!("a{}", depth + 1);
        ranked.push(row(
            HarnessId::ClaudeCode,
            &format!("a{depth}"),
            50 - depth,
            Some(&parent),
        ));
    }
    let kept = with_ancestors(ranked, 1);
    assert_eq!(kept.len(), 9);
    assert_eq!(
        ids(&kept),
        vec!["leaf", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8"]
    );
}

#[test]
fn with_ancestors_terminates_on_cycles() {
    let ranked = vec![
        row(HarnessId::ClaudeCode, "a", 10, Some("b")),
        row(HarnessId::ClaudeCode, "z", 9, None),
        row(HarnessId::ClaudeCode, "b", 1, Some("a")),
    ];
    assert_eq!(ids(&with_ancestors(ranked, 2)), vec!["a", "z", "b"]);
    let unchanged = vec![row(HarnessId::ClaudeCode, "a", 1, Some("a"))];
    assert_eq!(ids(&with_ancestors(unchanged, 0)), vec!["a"]);
}

fn unprompted_fixture(root: &Path) -> Result<StoreRoots, Box<dyn std::error::Error>> {
    write(
        root,
        "kimi/sessions/wd_x/session_probe/state.json",
        r#"{"cwd":"/w","updatedAt":2,"isCustomTitle":false}"#,
    )?;
    write(
        root,
        "kimi/sessions/wd_x/session_probe/agents/main/wire.jsonl",
        r#"{"type":"metadata"}"#,
    )?;
    write(
        root,
        "kimi/sessions/wd_x/session_used/state.json",
        r#"{"updatedAt":1}"#,
    )?;
    write(
        root,
        "kimi/sessions/wd_x/session_used/agents/main/wire.jsonl",
        &kimi_append("user", r#""hello""#),
    )?;
    write(
        root,
        &format!("grok/sessions/enc/{U1}/summary.json"),
        &format!(r#"{{"info":{{"id":"{U1}"}},"session_summary":"","num_chat_messages":2}}"#),
    )?;
    write(
        root,
        &format!("grok/sessions/enc/{U1}/chat_history.jsonl"),
        concat!(
            r#"{"type":"system","content":"s"}"#,
            "\n",
            r#"{"type":"user","content":"r","synthetic_reason":"system_reminder"}"#,
        ),
    )?;
    write(
        root,
        &format!("grok/sessions/enc/{U2}/summary.json"),
        &format!(r#"{{"info":{{"id":"{U2}"}},"session_summary":""}}"#),
    )?;
    write(
        root,
        &format!("grok/sessions/enc/{U2}/chat_history.jsonl"),
        r#"{"type":"user","content":"<user_query>hello</user_query>"}"#,
    )?;
    write(
        root,
        &format!("codex/sessions/2026/10/01/rollout-2026-10-01T00-00-00-{U1}.jsonl"),
        r#"{"type":"session_meta","payload":{"cwd":"/w"}}"#,
    )?;
    write(
        root,
        &format!("codex/sessions/2026/10/01/rollout-2026-10-01T00-00-01-{U2}.jsonl"),
        &codex_line("hello"),
    )?;
    Ok(StoreRoots {
        kimi: Some(root.join("kimi")),
        grok: Some(root.join("grok")),
        codex: Some(root.join("codex")),
        ..StoreRoots::default()
    })
}

#[tokio::test]
async fn sessions_without_a_user_turn_are_described_but_not_listed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lister = lister(unprompted_fixture(dir.path())?);
    let mut rows: Vec<_> = lister
        .list(&capture::listing::LISTING_ORDER, ListLimit::DEFAULT)
        .await
        .into_iter()
        .map(|row| (row.harness, row.id))
        .collect();
    rows.sort();
    let mut expected = vec![
        (HarnessId::GrokBuild, U2.to_owned()),
        (HarnessId::KimiCode, "session_used".to_owned()),
        (HarnessId::Codex, U2.to_owned()),
    ];
    expected.sort();
    assert_eq!(rows, expected);
    for (harness, id) in [
        (HarnessId::KimiCode, "session_probe"),
        (HarnessId::GrokBuild, U1),
        (HarnessId::Codex, U1),
    ] {
        assert!(lister.describe(harness, id, None).await?.is_some());
    }
    Ok(())
}

fn mixed_fixture(root: &Path) -> Result<StoreRoots, Box<dyn std::error::Error>> {
    let claude = write(
        root,
        "claude/projects/-home-a/tied.jsonl",
        r#"{"type":"user","message":{"content":"c"}}"#,
    )?;
    set_mtime(&claude, 1_700_000_000_000)?;
    let connection = hermes_db(&root.join("hermes"))?;
    connection.execute_batch(
        "INSERT INTO sessions VALUES ('h-tied', 1700000000, NULL);\
         INSERT INTO sessions VALUES ('h-new', 1800000000, NULL);\
         INSERT INTO messages VALUES ('h-tied', 'user', 'a', 1);\
         INSERT INTO messages VALUES ('h-new', 'user', 'b', 1);",
    )?;
    for index in 0..7_u64 {
        let rollout = write(
            root,
            &format!(
                "codex/sessions/2026/10/01/rollout-x-00000000-0000-4000-8000-00000000000{index}.jsonl"
            ),
            &codex_line("ask"),
        )?;
        set_mtime(&rollout, 1_600_000_000_000 + index)?;
    }
    Ok(StoreRoots {
        claude: Some(root.join("claude")),
        hermes: Some(root.join("hermes")),
        codex: Some(root.join("codex")),
        ..StoreRoots::default()
    })
}

#[tokio::test]
async fn assembly_sorts_globally_and_breaks_ties_by_harness_order() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lister = lister(mixed_fixture(dir.path())?);
    let rows = lister
        .list(
            &[HarnessId::Hermes, HarnessId::ClaudeCode],
            ListLimit::DEFAULT,
        )
        .await;
    assert_eq!(ids(&rows), vec!["h-new", "tied", "h-tied"]);
    Ok(())
}

#[tokio::test]
async fn assembly_scans_only_the_roster_harnesses() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lister = lister(mixed_fixture(dir.path())?);
    let rows = lister
        .list(
            &[HarnessId::ClaudeCode, HarnessId::Hermes],
            ListLimit::DEFAULT,
        )
        .await;
    assert!(rows.iter().all(|row| row.harness != HarnessId::Codex));
    let everything = lister
        .list(&capture::listing::LISTING_ORDER, ListLimit::DEFAULT)
        .await;
    assert_eq!(
        everything
            .iter()
            .filter(|row| row.harness == HarnessId::Codex)
            .count(),
        7
    );
    Ok(())
}

#[tokio::test]
async fn assembly_applies_the_limit_and_the_per_harness_route_ignores_query_limits() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lister = lister(mixed_fixture(dir.path())?);
    let limited = lister
        .list(&[HarnessId::Codex], ListLimit::from_query(Some("5")))
        .await;
    assert_eq!(limited.len(), 5);
    let per_harness = lister
        .list(&[HarnessId::Codex], ListLimit::per_harness_route())
        .await;
    assert_eq!(per_harness.len(), 7);
    Ok(())
}

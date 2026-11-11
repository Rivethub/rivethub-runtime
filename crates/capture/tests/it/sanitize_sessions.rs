use std::path::Path;

use capture::share;
use capture::share::BundlePaths;
use capture::share::Clobber;
use capture::share::Outcome;
use chrono::DateTime;
use chrono::Utc;
use config::MappingFile;
use kernel::memory::share::Choice;
use kernel::memory::share::SessionRedaction;
use kernel::memory::share::Term;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CLIENT: &str = "Falcon Labs";
const GITHUB: &str = concat!("ghp_", "Pl4nTedSessN8RmT2vLx9WcN4bKp7HyJ3dFs6");
const OUTPUT: &str = "payroll.csv: 41 rows, totals 88120";

fn at() -> TestResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339("2026-11-10T12:00:00Z")?.with_timezone(&Utc))
}

fn said(event_id: &str, content: String) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn tool(event_id: &str) -> TestResult<CaptureMessage> {
    let arguments: JsonObject = serde_json::from_str(&format!(
        r#"{{"command": "export GITHUB_TOKEN={GITHUB} && cat payroll.csv", "{CLIENT}": "dir"}}"#
    ))?;
    Ok(CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::Assistant,
        content: "running it".to_owned(),
        tool_name: Some("Bash".to_owned()),
        tool_args: Some(arguments),
        tool_result: Some(OUTPUT.to_owned()),
        metadata: None,
        created_at: None,
    })
}

fn session(n: u8) -> TestResult<BundledSession> {
    let key = format!("claude-code:s{n}");
    Ok(BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: format!("00000000-0000-4000-8000-00000000000{n}"),
            session_key: key.clone(),
            agent: "claude-code".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: key,
            agent: "claude-code".to_owned(),
            channel: None,
            title: Some(format!("{CLIENT} rollout")),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![
                said(
                    &format!("evt-{n}a"),
                    format!("we paired with {CLIENT} and falcon labs"),
                ),
                tool(&format!("evt-{n}b"))?,
            ],
        },
    })
}

fn redact(terms: &[&str], drop_tool_outputs: bool) -> TestResult<Choice> {
    let terms = terms
        .iter()
        .map(|term| Term::try_from((*term).to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Choice::Redact(SessionRedaction::new(
        terms,
        drop_tool_outputs,
    )?))
}

async fn sanitized(
    directory: &Path,
    key: &Path,
    choices: Vec<Choice>,
) -> TestResult<(Outcome, String)> {
    let sessions = choices
        .into_iter()
        .zip(1..)
        .map(|(choice, n)| Ok((session(n)?, choice)))
        .collect::<TestResult<Vec<_>>>()?;
    let chosen = share::chosen(
        BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        sessions,
        at()?,
    )?;
    let paths = BundlePaths::beside(&directory.join("work.clean.json"));
    let outcome = share::sanitize(share::Job {
        bundle: &chosen.bundle,
        sessions: &chosen.sessions,
        mapping: &MappingFile::Default(directory.join("absent.toml")),
        key,
        decider: None,
        paths: &paths,
        clobber: Clobber::Refuse,
        at: at()?,
    })
    .await?;
    let report = std::fs::read_to_string(&paths.report)?;
    Ok((outcome, report))
}

fn texts(session: &BundledSession) -> TestResult<String> {
    Ok(serde_json::to_string(session)?)
}

#[tokio::test]
async fn a_session_term_redacts_only_its_session_and_only_that_text() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (outcome, report) = sanitized(
        directory.path(),
        &directory.path().join("sanitize.key"),
        vec![redact(&[CLIENT], false)?, Choice::Approve],
    )
    .await?;

    let [chosen, approved] = outcome.sanitized.sessions.as_slice() else {
        return Err("two sessions".into());
    };
    assert_eq!(
        chosen
            .batch
            .messages
            .first()
            .map(|message| &message.content),
        Some(&"we paired with [REDACTED:session_term] and falcon labs".to_owned())
    );
    assert_eq!(
        chosen.batch.title.as_deref(),
        Some("[REDACTED:session_term] rollout")
    );
    assert!(!texts(chosen)?.contains(CLIENT));
    assert!(texts(chosen)?.contains(OUTPUT));
    assert_eq!(
        approved.batch.title.as_deref(),
        Some(format!("{CLIENT} rollout").as_str())
    );
    assert!(texts(approved)?.contains(&format!("\"{CLIENT}\"")));
    assert_eq!(
        share::rule_counts(&outcome.report),
        "session_term 3, github_token 2"
    );
    assert!(report.contains("| mapping | session_term | `[REDACTED:session_term]` | 3 |"));
    assert!(!report.contains(CLIENT), "{report}");
    assert_eq!(capture::sanitize::secrets(&outcome.sanitized), Vec::new());
    Ok(())
}

#[tokio::test]
async fn drop_tool_outputs_replaces_only_that_sessions_tool_results() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (outcome, report) = sanitized(
        directory.path(),
        &directory.path().join("sanitize.key"),
        vec![Choice::Approve, redact(&[], true)?],
    )
    .await?;

    let [approved, dropped] = outcome.sanitized.sessions.as_slice() else {
        return Err("two sessions".into());
    };
    let call = dropped.batch.messages.get(1).ok_or("tool call")?;
    assert_eq!(
        call.tool_result.as_deref(),
        Some("[REDACTED:drop_tool_outputs]")
    );
    assert_eq!(call.tool_name.as_deref(), Some("Bash"));
    let arguments = serde_json::to_string(&call.tool_args)?;
    assert!(arguments.contains("cat payroll.csv"), "{arguments}");
    assert!(!arguments.contains(GITHUB), "{arguments}");
    assert_eq!(
        approved
            .batch
            .messages
            .get(1)
            .and_then(|message| message.tool_result.as_deref()),
        Some(OUTPUT)
    );
    assert_eq!(
        share::rule_counts(&outcome.report),
        "github_token 2, drop_tool_outputs 1"
    );
    assert!(!report.contains(OUTPUT), "{report}");
    assert_eq!(capture::sanitize::secrets(&outcome.sanitized), Vec::new());
    Ok(())
}

#[tokio::test]
async fn the_policy_hash_changes_with_the_terms_but_never_names_them() -> TestResult {
    let state = tempfile::tempdir()?;
    let key = state.path().join("sanitize.key");
    let mut hashes = Vec::new();
    for choice in [
        Choice::Approve,
        Choice::Approve,
        redact(&[CLIENT], false)?,
        redact(&["falcon labs"], false)?,
    ] {
        let directory = tempfile::tempdir()?;
        let (outcome, report) = sanitized(directory.path(), &key, vec![choice]).await?;
        assert!(!report.contains("falcon labs"), "{report}");
        hashes.push(outcome.report.policy_sha256);
    }
    hashes.dedup();
    assert_eq!(hashes.len(), 3, "{hashes:?}");
    Ok(())
}

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";

const SCHEMA: &str = "\
CREATE TABLE message (\
    id TEXT PRIMARY KEY,\
    session_id TEXT NOT NULL,\
    role TEXT NOT NULL,\
    created_at TEXT NOT NULL\
);\
CREATE TABLE part (\
    id TEXT PRIMARY KEY,\
    message_id TEXT NOT NULL,\
    session_id TEXT NOT NULL,\
    type TEXT NOT NULL,\
    text TEXT,\
    tool_name TEXT,\
    tool_arguments TEXT,\
    tool_result TEXT,\
    created_at TEXT NOT NULL\
);";

type TestResult = Result<(), Box<dyn std::error::Error>>;

type Part = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

const PARTS: [Part; 4] = [
    (
        "p1",
        "m1",
        "text",
        Some("list the files"),
        None,
        None,
        None,
        "2026-10-14T12:00:00Z",
    ),
    (
        "p2",
        "m2",
        "text",
        Some("Running ls."),
        None,
        None,
        None,
        "2026-10-14T12:00:01Z",
    ),
    (
        "p3",
        "m2",
        "tool",
        None,
        Some("bash"),
        Some(r#"{"cmd":"ls"}"#),
        Some("a.txt"),
        "2026-10-14T12:00:02Z",
    ),
    (
        "p4",
        "m2",
        "text",
        Some("One file: a.txt."),
        None,
        None,
        None,
        "2026-10-14T12:00:03Z",
    ),
];

fn store() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let connection = rusqlite::Connection::open(dir.path().join("opencode.db"))?;
    connection.execute_batch(SCHEMA)?;
    let message = "INSERT INTO message (id, session_id, role, created_at) VALUES (?1, ?2, ?3, ?4)";
    connection.execute(message, ("m1", SESSION, "user", "2026-10-14T12:00:00Z"))?;
    connection.execute(
        message,
        ("m2", SESSION, "assistant", "2026-10-14T12:00:01Z"),
    )?;
    let part = "INSERT INTO part (id, message_id, session_id, type, text, tool_name, tool_arguments, tool_result, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    for (id, owner, kind, text, name, arguments, result, at) in PARTS {
        connection.execute(
            part,
            (id, owner, SESSION, kind, text, name, arguments, result, at),
        )?;
    }
    Ok(dir)
}

#[tokio::test]
async fn an_opencode_1_tool_part_is_a_finished_call_between_what_was_said() -> TestResult {
    let dir = store()?;
    let lister = StoreSessionLister::new(StoreRoots {
        opencode: Some(dir.path().to_path_buf()),
        ..StoreRoots::default()
    });
    let TranscriptRead::Read(read) = lister
        .transcript(HarnessId::Opencode, SESSION, TranscriptWindow::Latest)
        .await?
    else {
        return Err("opencode has a transcript source".into());
    };
    let [prompt, work, reply] = read.turns.as_slice() else {
        return Err(format!("expected three turns, got {:?}", read.turns).into());
    };
    assert_eq!(prompt.role, SessionMessageRole::User);
    assert_eq!(prompt.text, "list the files");
    assert_eq!(work.text, "Running ls.");
    let [call] = work.tools.as_deref().unwrap_or_default() else {
        return Err(format!("expected one call, got {work:?}").into());
    };
    assert_eq!(call.name, "bash");
    assert_eq!(call.status, TranscriptToolStatus::Done);
    let id = call.id.as_deref().ok_or("the call has an id")?;
    let ToolOutputRead::Found(output) =
        lister.tool_output(HarnessId::Opencode, SESSION, id).await?
    else {
        return Err("the call has an output".into());
    };
    assert_eq!(output.text, "a.txt");
    assert_eq!(reply.text, "One file: a.txt.");
    assert_eq!(reply.tools, None);
    Ok(())
}

use std::path::Path;
use std::path::PathBuf;

use capture::listing::StoreRoots;
use capture::listing::StoreSessionLister;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptToolStatus;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const SESSION: &str = "33333333-3333-3333-3333-333333333333";

fn place(root: &Path, relative: &str, fixture: &str) -> TestResult {
    let destination = root.join(relative);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(testkit::recording(fixture), &destination)?;
    Ok(())
}

struct Recorded {
    harness: HarnessId,
    file: String,
    fixture: &'static str,
    roots: fn(PathBuf) -> StoreRoots,
}

fn recorded() -> [Recorded; 3] {
    [
        Recorded {
            harness: HarnessId::Omp,
            file: format!("sessions/-work-demo/2026-10-14T12-00-00-000Z_{SESSION}.jsonl"),
            fixture: "capture/stores/omp.jsonl",
            roots: |omp| StoreRoots {
                omp: Some(omp),
                ..StoreRoots::default()
            },
        },
        Recorded {
            harness: HarnessId::Cursor,
            file: format!("projects/work-demo/agent-transcripts/{SESSION}/{SESSION}.jsonl"),
            fixture: "capture/stores/cursor.jsonl",
            roots: |cursor| StoreRoots {
                cursor: Some(cursor),
                ..StoreRoots::default()
            },
        },
        Recorded {
            harness: HarnessId::KimiCode,
            file: format!("sessions/wd_demo_hash/session_{SESSION}/agents/main/wire.jsonl"),
            fixture: "capture/stores/kimi-wire.jsonl",
            roots: |kimi| StoreRoots {
                kimi: Some(kimi),
                ..StoreRoots::default()
            },
        },
    ]
}

async fn turns(
    lister: &StoreSessionLister,
    harness: HarnessId,
) -> TestResult<Vec<HarnessTranscriptTurn>> {
    match lister
        .transcript(harness, SESSION, TranscriptWindow::Latest)
        .await?
    {
        TranscriptRead::Read(read) => Ok(read.turns),
        TranscriptRead::NoSource => Err(format!("{harness} has a transcript source").into()),
    }
}

#[tokio::test]
async fn omp_cursor_and_kimi_sessions_are_read_with_their_tool_call() -> TestResult {
    for store in recorded() {
        let harness = store.harness;
        let dir = tempfile::tempdir()?;
        place(dir.path(), &store.file, store.fixture)?;
        let lister = StoreSessionLister::new((store.roots)(dir.path().to_path_buf()));
        let turns = turns(&lister, harness).await?;
        let [prompt, work] = turns.as_slice() else {
            return Err(format!("{harness}: expected a prompt and the work, got {turns:?}").into());
        };
        assert_eq!(prompt.role, SessionMessageRole::User, "{harness}");
        assert_eq!(prompt.text, "list the files", "{harness}");
        assert_eq!(work.text, "Running ls.", "{harness}");
        let [call] = work.tools.as_deref().unwrap_or_default() else {
            return Err(format!("{harness}: expected one call, got {work:?}").into());
        };
        assert_eq!(call.name, "bash", "{harness}");
        assert_eq!(call.status, TranscriptToolStatus::Done, "{harness}");
        let id = call.id.as_deref().ok_or("the call has an id")?;
        let ToolOutputRead::Found(output) = lister.tool_output(harness, SESSION, id).await? else {
            return Err(format!("{harness}: the call has an output").into());
        };
        assert_eq!(output.text, "a.txt", "{harness}");
    }
    Ok(())
}

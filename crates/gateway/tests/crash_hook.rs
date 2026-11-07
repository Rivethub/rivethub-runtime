use std::process::Command;

use gateway::account::{AccountFiles, CrashReport, queued};

const HOME_PATH: &str = "/home/user/projects/acmekit/notes.md";
const SESSION_TEXT: &str = "the user asked about the AcmeKit launch plan";

#[test]
fn a_panic_with_a_home_path_and_session_text_queues_a_report_with_neither()
-> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::tempdir()?;
    let files = AccountFiles::new(state.path());
    let install_id = files.open()?.install_id;
    let crashed = Command::new(env!("CARGO_BIN_EXE_crash-fixture"))
        .arg(state.path())
        .arg(format!("cannot open {HOME_PATH}: {SESSION_TEXT}"))
        .env("RUST_BACKTRACE", "0")
        .output()?;
    assert!(!crashed.status.success());
    assert!(String::from_utf8_lossy(&crashed.stderr).contains(SESSION_TEXT));
    let waiting = queued(&files)?;
    assert_eq!(waiting.len(), 1);
    let text = std::fs::read_to_string(waiting.first().ok_or("no report")?)?;
    for leaked in ["/home/", "acmekit", "AcmeKit", SESSION_TEXT, "cannot open"] {
        assert!(!text.contains(leaked), "{leaked} in {text}");
    }
    let report: CrashReport = serde_json::from_str(&text)?;
    assert_eq!(report.install_id, install_id);
    assert_eq!(
        report.location.rsplit_once(':').map(|(module, _)| module),
        Some("gateway::crash-fixture::crash")
    );
    assert!(!report.frames.is_empty(), "{text}");
    assert!(report.frames.iter().all(|frame| frame.len() <= 160));
    Ok(())
}

#[test]
fn a_panic_with_crash_reports_off_queues_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::tempdir()?;
    let files = AccountFiles::new(state.path());
    let mut install = files.open()?;
    install.crash_reports = false;
    files.save(&install)?;
    let crashed = Command::new(env!("CARGO_BIN_EXE_crash-fixture"))
        .arg(state.path())
        .arg("boom")
        .output()?;
    assert!(!crashed.status.success());
    assert_eq!(queued(&files)?, Vec::<std::path::PathBuf>::new());
    Ok(())
}

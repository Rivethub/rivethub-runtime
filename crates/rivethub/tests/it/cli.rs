use std::time::Duration;

use tokio::process::Command;

#[tokio::test]
async fn binary_dispatch_preserves_stdout_stderr_and_exit_codes() {
    let home = tempfile::tempdir().expect("isolated home");
    for (args, code, stdout, stderr) in [
        (vec![], 0, "RivetHub operator commands", ""),
        (vec!["help"], 0, "Commands:", ""),
        (vec!["version"], 0, "RivetHub v", ""),
        (vec!["doctor", "--help"], 0, "--json", ""),
        (
            vec!["unknown-command"],
            1,
            "",
            "Unknown command: unknown-command",
        ),
        (
            vec!["config", "path"],
            1,
            "",
            "No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or set RIVETHUB_CONFIG",
        ),
        (
            vec!["stop"],
            0,
            "No running instance found (no PID file).",
            "",
        ),
        (vec!["status"], 0, "not running", "Health unavailable"),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(env!("CARGO_BIN_EXE_rh"))
                .args(&args)
                .env_clear()
                .env("HOME", home.path())
                .env("TOKIO_WORKER_THREADS", "2")
                .kill_on_drop(true)
                .current_dir(home.path())
                .output(),
        )
        .await
        .map_err(|error| format!("rh {args:?} exceeded the command deadline: {error}"))
        .expect("command deadline")
        .expect("rh");
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        let actual_stdout = String::from_utf8(output.stdout).expect("stdout");
        let actual_stderr = String::from_utf8(output.stderr).expect("stderr");
        if stdout.is_empty() {
            assert!(actual_stdout.is_empty(), "{args:?}: {actual_stdout}");
        } else {
            assert!(actual_stdout.contains(stdout), "{args:?}: {actual_stdout}");
        }
        if stderr.is_empty() {
            assert!(actual_stderr.is_empty(), "{args:?}: {actual_stderr}");
        } else {
            assert!(actual_stderr.contains(stderr), "{args:?}: {actual_stderr}");
        }
    }
}

#[tokio::test]
async fn one_release_compatibility_binary_dispatches_the_same_tree() {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_rivetos"))
            .arg("version")
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("alias deadline")
    .expect("compatibility alias");
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .expect("version")
            .starts_with("RivetHub v")
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
}

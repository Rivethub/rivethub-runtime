use std::time::Duration;

use tokio::process::Command;

#[tokio::test]
async fn binary_dispatch_preserves_stdout_stderr_and_exit_codes() {
    let home = tempfile::tempdir().expect("isolated home");
    for (args, code, stdout, stderr) in [
        (["version"], 0, "RivetHub v", ""),
        (
            ["unknown-command"],
            1,
            "",
            "Unknown command: unknown-command",
        ),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(env!("CARGO_BIN_EXE_rh"))
                .args(args)
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
async fn the_rivetos_binary_dispatches_the_same_tree() {
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

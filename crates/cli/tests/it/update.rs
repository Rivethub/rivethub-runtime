#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

#[path = "update/cloud.rs"]
mod cloud;
#[path = "update/source.rs"]
mod source;
#[path = "update_support/mod.rs"]
mod update_support;

use std::os::unix::fs::symlink;

use cli::Exit;
use cli::service::Scope;
use cli::update::{Channel, Updater};
use tokio_util::sync::CancellationToken;
use update_support::{BINARY, Fixture, MIGRATION, Published, Result, args, server};

#[tokio::test]
async fn stable_verifies_redirected_archive_migrates_swaps_and_restarts() -> Result {
    let fixture = Fixture::new()?;
    let report = fixture
        .run(&args(), &fixture.env(), Published::Good)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    assert_eq!(
        std::fs::read(fixture.binary.with_extension("previous"))?,
        b"old runtime"
    );
    let directory = fixture.binary.parent().ok_or("directory")?;
    assert_eq!(
        std::fs::read(directory.join("rivethub-migrate"))?,
        MIGRATION
    );
    assert_eq!(
        std::fs::read(directory.join("rivethub-migrate.previous"))?,
        b"old migration"
    );
    let calls = std::fs::read_to_string(&fixture.calls)?;
    let lines: Vec<_> = calls.lines().collect();
    assert!(
        lines
            .first()
            .ok_or("migration")?
            .starts_with("migrate|sqlite|--path|")
    );
    assert_eq!(lines.get(1), Some(&"systemctl|--user|daemon-reload"));
    assert_eq!(lines.get(2), Some(&"systemctl|--user|restart|rivethub"));
    assert_eq!(
        lines.get(3),
        Some(&"systemctl|--user|status|rivethub|--no-pager")
    );
    Ok(())
}

#[tokio::test]
async fn bad_archives_and_migration_failures_preserve_installation() -> Result {
    for (published, failure, message) in [
        (
            Published::BadChecksum,
            "",
            "SHA-256 verification failed for rh-0.9.0-",
        ),
        (
            Published::MissingMigration,
            "",
            "exactly one regular file named rivethub-migrate",
        ),
        (Published::Good, "migration", "migration failed"),
    ] {
        let fixture = Fixture::new()?;
        let env = fixture.env().with_override("RIVETHUB_TEST_FAIL", failure);
        let report = fixture.run(&args(), &env, published).await?;
        assert_eq!(report.exit, Exit::Failure);
        assert!(report.stderr.contains(message), "{}", report.stderr);
        assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
        assert!(!fixture.binary.with_extension("previous").exists());
        let calls = std::fs::read_to_string(&fixture.calls)?;
        assert!(!calls.contains("systemctl"));
        if failure.is_empty() {
            assert_eq!(calls, "");
        }
    }
    Ok(())
}

#[tokio::test]
async fn no_restart_repairs_a_generated_unit_and_leaves_a_hand_edit() -> Result {
    let fixture = Fixture::new()?;
    let unit = Scope::User.unit_path(fixture.home.path());
    let env_file = fixture.home.path().join(".rivethub/.env");
    let legacy = format!(
        "[Unit]\nDescription=RivetHub\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=\"/opt/rh\" local\nEnvironmentFile=-\"{}\"\nEnvironment=RIVETHUB_LOG_LEVEL=info\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        env_file.display()
    );
    std::fs::write(&unit, &legacy)?;
    let mut args = args();
    args.version = Some("v0.9.0".to_owned());
    args.no_restart = true;
    let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report
            .stdout
            .contains("EnvironmentFile is no longer quoted"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.contains("added RestartPreventExitStatus=11"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.contains("Restart skipped (--no-restart).\n"),
        "{}",
        report.stdout
    );
    let updated = std::fs::read_to_string(&unit)?;
    assert!(!updated.contains("EnvironmentFile=-\""));
    assert!(updated.contains("RestartPreventExitStatus=11\n"));
    assert_eq!(
        std::fs::read_to_string(unit.with_file_name("rivethub.service.bak"))?,
        legacy
    );
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(
        calls.contains("systemctl|--user|daemon-reload\n"),
        "{calls}"
    );
    assert!(!calls.contains("|restart|"), "{calls}");

    let fixture = Fixture::new()?;
    let unit = Scope::User.unit_path(fixture.home.path());
    let original = std::fs::read_to_string(&unit)?;
    let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report.stdout.contains("edited by hand"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.contains("`rh service init`"),
        "{}",
        report.stdout
    );
    assert_eq!(std::fs::read_to_string(&unit)?, original);
    assert!(!std::fs::read_to_string(&fixture.calls)?.contains("systemctl"));
    Ok(())
}

#[tokio::test]
async fn explicit_version_and_no_restart_still_migrate() -> Result {
    let fixture = Fixture::new()?;
    let mut args = args();
    args.version = Some("v0.9.0".to_owned());
    args.no_restart = true;
    let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(calls.starts_with("migrate"));
    assert!(!calls.contains("systemctl"));
    Ok(())
}

#[tokio::test]
async fn an_explicit_version_pins_a_prerelease_and_drafts_are_refused() -> Result {
    for (channel, tag, outcome) in [
        (Channel::Stable, "v0.10.0", Ok(())),
        (Channel::Beta, "v0.10.0", Ok(())),
        (
            Channel::Stable,
            "v0.11.0",
            Err("release v0.11.0 is a draft, not a published release"),
        ),
        (
            Channel::Stable,
            "v0.12.0",
            Err("no published release is tagged v0.12.0"),
        ),
    ] {
        let fixture = Fixture::new()?;
        let mut args = args();
        args.channel = channel;
        args.version = Some(tag.to_owned());
        args.no_restart = true;
        let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
        match outcome {
            Ok(()) => assert_eq!(report.exit, Exit::Ok, "{}", report.stderr),
            Err(message) => {
                assert_eq!(report.exit, Exit::Failure);
                assert!(report.stderr.contains(message), "{}", report.stderr);
            }
        }
        let installed = std::fs::read(&fixture.binary)? == BINARY;
        assert_eq!(installed, outcome.is_ok());
    }
    Ok(())
}

#[tokio::test]
async fn an_unpinned_update_never_installs_an_older_or_the_same_release() -> Result {
    for (channel, running, message) in [
        (
            Channel::Stable,
            "0.12.0",
            "rh 0.12.0 is already newer than the latest stable release v0.9.0; nothing to do.\n",
        ),
        (
            Channel::Beta,
            "0.12.0",
            "rh 0.12.0 is already newer than the latest beta release v0.10.0; nothing to do.\n",
        ),
        (
            Channel::Stable,
            "0.9.0",
            "rh 0.9.0 is already up to date (latest stable release v0.9.0).\n",
        ),
    ] {
        let fixture = Fixture::new()?;
        let mut args = args();
        args.channel = channel;
        let report = fixture
            .run_as(&args, &fixture.env(), Published::Good, running)
            .await?;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert!(report.stdout.ends_with(message), "{}", report.stdout);
        assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
        assert_eq!(std::fs::read_to_string(&fixture.calls)?, "");
    }
    let fixture = Fixture::new()?;
    let mut args = args();
    args.version = Some("v0.9.0".to_owned());
    args.no_restart = true;
    let report = fixture
        .run_as(&args, &fixture.env(), Published::Good, "0.12.0")
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    Ok(())
}

#[tokio::test]
async fn without_a_unit_and_without_a_running_node_nothing_is_restarted() -> Result {
    let fixture = Fixture::new()?;
    std::fs::remove_file(Scope::User.unit_path(fixture.home.path()))?;
    let report = fixture
        .run(&args(), &fixture.env(), Published::Good)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report
            .stdout
            .ends_with("No RivetHub node is running, so none needed a restart.\n"),
        "{}",
        report.stdout
    );
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(!calls.contains("systemctl"));
    assert!(!calls.contains("rh|"));
    Ok(())
}

#[tokio::test]
async fn without_a_unit_a_local_node_is_stopped_and_started_with_the_new_binary() -> Result {
    let fixture = Fixture::new()?;
    std::fs::remove_file(Scope::User.unit_path(fixture.home.path()))?;
    let (pid, node) = fake_node(&fixture, &["local", "--port", "6123", "--no-lan"])?;
    let report = fixture
        .run(&args(), &fixture.env(), Published::Good)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report
            .stdout
            .contains(&format!("Stopping the local node (PID {pid})...\n"))
    );
    assert!(
        report
            .stdout
            .ends_with("Started RivetHub local node at http://127.0.0.1:6123\n"),
        "{}",
        report.stdout
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), node).await???;
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(!calls.contains("systemctl"));
    assert!(
        calls.ends_with("rh|local|--ensure|--wait|30|--port|6123|--no-lan\n"),
        "{calls}"
    );
    Ok(())
}

#[tokio::test]
async fn without_a_unit_a_node_rh_local_did_not_start_is_left_running() -> Result {
    let fixture = Fixture::new()?;
    std::fs::remove_file(Scope::User.unit_path(fixture.home.path()))?;
    let (pid, node) = fake_node(&fixture, &["start"])?;
    let report = fixture
        .run(&args(), &fixture.env(), Published::Good)
        .await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(report.stdout.ends_with(&format!(
        "The RivetHub node running as PID {pid} was not started by `rh local`; restart it to run the new binary.\n"
    )));
    assert!(!node.is_finished());
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid)?),
        nix::sys::signal::Signal::SIGTERM,
    )?;
    tokio::time::timeout(std::time::Duration::from_secs(10), node).await???;
    assert!(!std::fs::read_to_string(&fixture.calls)?.contains("rh|"));
    Ok(())
}

#[tokio::test]
async fn a_unit_path_that_is_not_a_file_is_named() -> Result {
    let fixture = Fixture::new()?;
    let unit = Scope::User.unit_path(fixture.home.path());
    std::fs::remove_file(&unit)?;
    std::fs::create_dir(&unit)?;
    let report = fixture
        .run(&args(), &fixture.env(), Published::Good)
        .await?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains(&unit.display().to_string()),
        "{}",
        report.stderr
    );
    assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
    Ok(())
}

fn fake_node(
    fixture: &Fixture,
    arguments: &[&str],
) -> Result<(
    u32,
    tokio::task::JoinHandle<std::io::Result<std::process::ExitStatus>>,
)> {
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("trap 'exit 0' TERM; while :; do sleep 0.05; done")
        .args(arguments)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().ok_or("node pid")?;
    std::fs::write(
        fixture.home.path().join(".rivethub/rivethub.pid"),
        format!("{pid}\n"),
    )?;
    Ok((pid, tokio::spawn(async move { child.wait().await })))
}

#[tokio::test]
async fn symlinks_out_and_traversal_are_refused() -> Result {
    let fixture = Fixture::new()?;
    let server = server(Published::Good).await?;
    let root = fixture.binary.parent().ok_or("root")?;
    let sibling = root.with_file_name("install 'quoted' $(literal)-sibling");
    std::fs::create_dir(&sibling)?;
    let outside = sibling.join("rh");
    std::fs::write(&outside, "outside")?;
    let link = root.join("linked");
    symlink(&outside, &link)?;
    for path in [link, root.join("../install 'quoted' $(literal)-sibling/rh")] {
        let updater = Updater::new(
            path,
            format!("http://{}/releases/", server.addr).parse()?,
            Scope::User,
        );
        let report = updater
            .run(&args(), &fixture.env(), &CancellationToken::new())
            .await;
        assert_eq!(report.exit, Exit::Failure);
        assert!(report.stderr.contains("outside its root"));
        assert_eq!(std::fs::read(&outside)?, b"outside");
    }
    server.close().await?;
    Ok(())
}

#[tokio::test]
async fn cancellation_and_busy_installations_refuse_changes() -> Result {
    let fixture = Fixture::new()?;
    let server = server(Published::Good).await?;
    let updater = Updater::new(
        fixture.binary.clone(),
        format!("http://{}/releases/", server.addr).parse()?,
        Scope::User,
    );
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let report = updater.run(&args(), &fixture.env(), &shutdown).await;
    assert!(report.stderr.contains("cancelled"));
    let lock = std::fs::File::create(
        fixture
            .binary
            .parent()
            .ok_or("parent")?
            .join(".rh-update.lock"),
    )?;
    lock.lock()?;
    let report = updater
        .run(&args(), &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("another update"));
    assert_eq!(std::fs::read(&fixture.calls)?, b"");
    server.close().await?;
    Ok(())
}

#[test]
fn update_db_recording_scope_has_no_unexplained_skips() -> Result {
    const SKIPPED: [(&str, &str); 0] = [];
    let files: std::collections::BTreeSet<_> = std::fs::read_dir(testkit::recording("cli"))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(
        files,
        std::iter::once(std::ffi::OsString::from("README.md")).collect(),
        "new CLI transcripts must be wired into their command's replay tests"
    );
    let skipped: std::collections::BTreeMap<_, _> = SKIPPED.into_iter().collect();
    for (name, reason) in &skipped {
        assert!(name.starts_with("update") || name.starts_with("db"));
        assert_ne!(*reason, "");
    }
    assert_eq!(skipped.len(), files.len() - 1);
    Ok(())
}

#[tokio::test]
async fn restart_failure_preserves_both_rollback_binaries() -> Result {
    let fixture = Fixture::new()?;
    let env = fixture.env().with_override("RIVETHUB_TEST_FAIL", "restart");
    let report = fixture.run(&args(), &env, Published::Good).await?;
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    assert_eq!(
        std::fs::read(fixture.binary.with_extension("previous"))?,
        b"old runtime"
    );
    assert_eq!(
        std::fs::read(fixture.binary.with_file_name("rivethub-migrate.previous"))?,
        b"old migration"
    );
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(calls.contains("systemctl|--user|restart"));
    assert!(!calls.contains("systemctl|--user|status"));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_download_cleans_staging_and_preserves_installation() -> Result {
    let fixture = Fixture::new()?;
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = CancellationToken::new();
    let listener_entered = std::sync::Arc::clone(&entered);
    let request_cancelled = release_request.clone();
    let route = axum::Router::new().route(
        "/releases/latest",
        axum::routing::get(move || {
            let entered = std::sync::Arc::clone(&listener_entered);
            let shutdown = request_cancelled.clone();
            async move {
                entered.notify_one();
                shutdown.cancelled().await;
                axum::http::StatusCode::REQUEST_TIMEOUT
            }
        }),
    );
    let server = testkit::stub_server(route).await?;
    let updater = Updater::new(
        fixture.binary.clone(),
        format!("http://{}/releases/", server.addr).parse()?,
        Scope::User,
    );
    let env = fixture.env();
    let shutdown = CancellationToken::new();
    let cancel = shutdown.clone();
    let task = tokio::spawn(async move { updater.run(&args(), &env, &cancel).await });
    let deadline = std::time::Duration::from_secs(10);
    tokio::time::timeout(deadline, entered.notified()).await?;
    shutdown.cancel();
    let report = tokio::time::timeout(deadline, task).await??;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("cancelled"));
    assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
    assert_eq!(std::fs::read(&fixture.calls)?, b"");
    let entries = std::fs::read_dir(fixture.binary.parent().ok_or("parent")?)?;
    for entry in entries {
        assert!(!entry?.file_type()?.is_dir());
    }
    release_request.cancel();
    server.close().await?;
    Ok(())
}

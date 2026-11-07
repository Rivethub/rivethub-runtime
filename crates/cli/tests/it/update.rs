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
async fn mesh_preflight_local_first_ssh_user_and_partial_failures() -> Result {
    let fixture = Fixture::new()?;
    fixture.mesh(&[
        "local.example.com",
        "reachable.example.com",
        "unreachable.example.com",
        "failed.example.com",
        "last.example.com",
    ])?;
    let mut args = args();
    args.mesh = true;
    args.ssh_user = Some("deploy".to_owned());
    args.version = Some("v0.9.0".to_owned());
    args.no_restart = true;
    let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("Unreachable: node-2"));
    assert!(report.stderr.contains("Update failed: node-3"));
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(!calls.contains("local.example.com"));
    let lines: Vec<_> = calls.lines().collect();
    assert!(lines.first().ok_or("preflight")?.ends_with("|true"));
    assert!(lines.get(4).ok_or("migration")?.starts_with("migrate|"));
    assert!(lines.last().ok_or("last peer")?.ends_with(
        "|-l|deploy|--|last.example.com|rh update --channel stable --version v0.9.0 --no-restart"
    ));
    assert!(!calls.contains("update --mesh"));
    assert_eq!(calls.matches("unreachable.example.com").count(), 1);
    Ok(())
}

#[tokio::test]
async fn beta_installs_the_highest_published_version_and_pins_mesh_peers() -> Result {
    let fixture = Fixture::new()?;
    fixture.mesh(&["local.example.com", "peer.example.com"])?;
    let mut args = args();
    args.channel = Channel::Beta;
    args.mesh = true;
    args.no_restart = true;
    let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report.stdout.contains("Installed v0.10.0;"),
        "{}",
        report.stdout
    );
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    let calls = std::fs::read_to_string(&fixture.calls)?;
    assert!(
        calls.lines().last().ok_or("peer")?.ends_with(
            "|--|peer.example.com|rh update --channel beta --version v0.10.0 --no-restart"
        )
    );
    Ok(())
}

#[tokio::test]
async fn only_beta_accepts_prerelease_tags_and_drafts_are_refused() -> Result {
    for (channel, tag, exit) in [
        (Channel::Stable, "v0.10.0", Exit::Failure),
        (Channel::Beta, "v0.10.0", Exit::Ok),
        (Channel::Beta, "v0.11.0", Exit::Failure),
    ] {
        let fixture = Fixture::new()?;
        let mut args = args();
        args.channel = channel;
        args.version = Some(tag.to_owned());
        args.no_restart = true;
        let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
        assert_eq!(report.exit, exit, "{}", report.stderr);
        let installed = std::fs::read(&fixture.binary)? == BINARY;
        assert_eq!(installed, exit == Exit::Ok);
    }
    Ok(())
}

#[tokio::test]
async fn malicious_mesh_input_fails_before_network_or_local_changes() -> Result {
    for host in [
        "-oProxyCommand=touch",
        "user@host",
        "host; touch marker",
        "$(touch marker)",
    ] {
        let fixture = Fixture::new()?;
        fixture.mesh(&["local.example.com", host])?;
        let mut args = args();
        args.mesh = true;
        let report = fixture.run(&args, &fixture.env(), Published::Good).await?;
        assert_eq!(report.exit, Exit::Failure);
        assert!(report.stderr.contains("invalid SSH"));
        assert_eq!(std::fs::read(&fixture.calls)?, b"");
        assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
    }
    Ok(())
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

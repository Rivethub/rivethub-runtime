use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use cli::Exit;
use cli::service::Scope;
use cli::update::Updater;
use tokio_util::sync::CancellationToken;

use super::update_support::{
    BINARY, Fixture, Published, Result, WebRelease, args, server_with_web, web_url,
};

#[derive(Clone, Copy)]
pub(super) enum Contents {
    Site,
    MissingIndex,
    Link,
    Parent,
    Oversize,
}

pub(super) fn build(
    version: &'static str,
    minimum: &'static str,
    contents: Contents,
) -> Result<WebRelease> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(u64::try_from(version.len())?);
    header.set_cksum();
    tar.append_data(
        &mut header,
        if matches!(contents, Contents::MissingIndex) {
            "readme.txt"
        } else {
            "./index.html"
        },
        version.as_bytes(),
    )?;
    let mut hostile = tar::Header::new_gnu();
    hostile.set_mode(0o644);
    hostile.set_size(0);
    match contents {
        Contents::Link => {
            hostile.set_entry_type(tar::EntryType::Symlink);
            tar.append_link(&mut hostile, "link", "../outside")?;
        }
        Contents::Parent => {
            hostile.as_mut_bytes()[..10].copy_from_slice(b"../outside");
            hostile.set_cksum();
            tar.append(&hostile, std::io::empty())?;
        }
        Contents::Oversize => {
            hostile.set_path("large.js")?;
            hostile.set_size(1024 * 1024 * 1024);
            hostile.set_cksum();
            tar.append(&hostile, std::io::empty())?;
        }
        Contents::Site | Contents::MissingIndex => {}
    }
    Ok(WebRelease {
        version,
        minimum,
        archive: tar.into_inner()?.finish()?,
        bad_digest: false,
        broken_manifest: false,
    })
}

pub(super) fn web_root(fixture: &Fixture) -> PathBuf {
    fixture.home.path().join(".rivethub/web")
}

pub(super) fn assert_active(fixture: &Fixture, version: &str) -> Result {
    let current = web_root(fixture).join("current");
    assert_eq!(fs::read_link(&current)?, Path::new(version));
    assert_eq!(fs::read_to_string(current.join("index.html"))?, version);
    Ok(())
}

fn updater(fixture: &Fixture, server: &testkit::Stub, running: &str) -> Result<Updater> {
    Ok(Updater::new(
        fixture.binary.clone(),
        format!("http://{}/releases/", server.addr).parse()?,
        web_url(server)?,
        Scope::User,
    )
    .running(running))
}

#[tokio::test]
async fn web_only_works_without_config_and_repairs_an_interrupted_install() -> Result {
    let fixture = Fixture::new()?;
    let config = fixture.home.path().join(".rivethub/config.yaml");
    fs::remove_file(&config)?;
    let root = web_root(&fixture);
    fs::create_dir_all(root.join("1.2.0"))?;
    fs::write(root.join("1.2.0/half-extracted"), "partial")?;
    fs::create_dir(root.join(".staging"))?;
    symlink("1.2.0", root.join("current"))?;

    let server = server_with_web(
        Published::Good,
        vec![build("1.2.0", "0.9.0", Contents::Site)?],
    )
    .await?;
    let updater = updater(&fixture, &server, "0.9.0")?;
    let mut args = args();
    args.web_only = true;
    for message in ["Web: Installed 1.2.0.\n", "Web: 1.2.0 is already active.\n"] {
        let report = updater
            .run(&args, &fixture.env(), &CancellationToken::new())
            .await;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert_eq!(report.stdout, message);
        assert_active(&fixture, "1.2.0")?;
    }
    assert!(!root.join("1.2.0/half-extracted").exists());
    assert!(!root.join(".staging").exists());
    assert!(!config.exists());
    assert_eq!(fs::read_to_string(&fixture.calls)?, "");
    assert_eq!(fs::read(&fixture.binary)?, b"old runtime");
    assert!(!fixture.binary.with_extension("previous").exists());
    server.close().await?;
    Ok(())
}

#[tokio::test]
async fn already_current_runtime_refreshes_web_and_downgrade_checks_installed_runtime() -> Result {
    let fixture = Fixture::new()?;
    let server = server_with_web(
        Published::Good,
        vec![
            build("1.0.0", "0.8.0", Contents::Site)?,
            build("1.2.0", "0.9.0", Contents::Site)?,
            build("2.0.0", "0.10.0", Contents::Site)?,
        ],
    )
    .await?;
    let report = updater(&fixture, &server, "0.9.0")?
        .run(&args(), &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(report.stdout.contains("already up to date"));
    assert!(report.stdout.contains("Web: Installed 1.2.0."));
    assert_active(&fixture, "1.2.0")?;
    assert_eq!(fs::read_to_string(&fixture.calls)?, "");

    let mut args = args();
    args.web_only = true;
    let report = updater(&fixture, &server, "0.10.0")?
        .run(&args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_active(&fixture, "2.0.0")?;

    args.web_only = false;
    args.version = Some("v0.9.0".to_owned());
    let report = updater(&fixture, &server, "0.10.0")?
        .run(&args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(report.stdout.contains("Installed v0.9.0"));
    assert!(report.stdout.contains("Web: Installed 1.2.0."));
    assert_active(&fixture, "1.2.0")?;
    server.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_bad_web_update_keeps_the_previous_bundle_and_the_new_runtime() -> Result {
    let fixture = Fixture::new()?;
    let first = server_with_web(
        Published::Good,
        vec![build("1.0.0", "0.8.0", Contents::Site)?],
    )
    .await?;
    let mut args = args();
    args.web_only = true;
    let report = updater(&fixture, &first, "0.8.3")?
        .run(&args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    first.close().await?;

    for (contents, bad_digest, error) in [
        (Contents::Site, true, "SHA-256"),
        (Contents::Link, false, "unsafe"),
        (Contents::Parent, false, "unsafe"),
        (Contents::Oversize, false, "too large"),
        (Contents::MissingIndex, false, "index.html"),
    ] {
        let mut bad = build("2.0.0", "0.8.0", contents)?;
        bad.bad_digest = bad_digest;
        let server = server_with_web(Published::Good, vec![bad]).await?;
        args.web_only = !bad_digest;
        let report = updater(&fixture, &server, "0.8.3")?
            .run(&args, &fixture.env(), &CancellationToken::new())
            .await;
        assert_eq!(report.exit, Exit::Failure);
        assert!(report.stderr.contains(error), "{}", report.stderr);
        assert!(report.stderr.contains("rh update --web-only"));
        if bad_digest {
            assert!(report.stdout.contains("Installed v0.9.0"));
        }
        assert_eq!(fs::read(&fixture.binary)?, BINARY);
        assert_active(&fixture, "1.0.0")?;
        assert!(!web_root(&fixture).join("2.0.0").exists());
        server.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_release_with_a_broken_manifest_is_skipped_for_the_next_best() -> Result {
    let fixture = Fixture::new()?;
    let mut broken = build("1.3.0", "0.9.0", Contents::Site)?;
    broken.broken_manifest = true;
    let server = server_with_web(
        Published::Good,
        vec![broken, build("1.2.0", "0.9.0", Contents::Site)?],
    )
    .await?;
    let updater = updater(&fixture, &server, "0.9.0")?;
    let mut args = args();
    args.web_only = true;
    let report = updater
        .run(&args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "Web: Installed 1.2.0.\n");
    assert_active(&fixture, "1.2.0")?;
    server.close().await?;
    Ok(())
}

#[tokio::test]
async fn explicit_static_dir_skips_the_feed_and_managed_storage() -> Result {
    let fixture = Fixture::new()?;
    let mut args = args();
    args.web_only = true;
    let updater = Updater::new(
        fixture.binary.clone(),
        "http://127.0.0.1:9/releases".parse()?,
        "http://127.0.0.1:9/web-releases".parse()?,
        Scope::User,
    );
    let explicit = fixture.home.path().join("explicit");
    let report = updater
        .run(
            &args,
            &fixture
                .env()
                .with_override("RIVETHUB_STATIC_DIR", &explicit),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stdout, "Web: Using explicit static_dir.\n");
    assert!(!web_root(&fixture).exists());
    Ok(())
}

#[tokio::test]
async fn web_only_leaves_legacy_state_available_for_first_start() -> Result {
    let home = tempfile::tempdir()?;
    let legacy = home.path().join(".rivetos");
    fs::create_dir(&legacy)?;
    let env = config::Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    let updater = Updater::new(
        home.path().join("rh"),
        "http://127.0.0.1:9/releases".parse()?,
        "http://127.0.0.1:9/web-releases".parse()?,
        Scope::User,
    );
    let mut args = args();
    args.web_only = true;
    let report = updater.run(&args, &env, &CancellationToken::new()).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("adopt its legacy state"));
    assert!(legacy.is_dir());
    assert!(!home.path().join(".rivethub").exists());
    Ok(())
}

#[tokio::test]
async fn web_signature_fails_closed_when_forged_and_accepts_trusted_key() -> Result {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use gateway::account::{ReleaseKeyring, UpdateKey};
    use sha2::{Digest as _, Sha256};

    let release = build("1.2.0", "0.9.0", Contents::Site)?;
    let manifest_json = format!(
        r#"{{"schemaVersion":1,"version":"1.2.0","commit":"{}","channel":"stable","minRhVersion":"0.9.0","asset":"rivethub-web-1.2.0.tar.gz","sha256":"{}"}}"#,
        "a".repeat(40),
        hex::encode(Sha256::digest(&release.archive))
    );
    let pair = Ed25519KeyPair::generate()?;
    let pub_raw: [u8; 32] = pair
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| "pubkey length")?;
    let keyring =
        ReleaseKeyring::default_trusted().with_key("test-key", UpdateKey::from_bytes(pub_raw));
    let good_sig = format!(
        "v1:test-key:{}\n",
        STANDARD.encode(pair.sign(manifest_json.as_bytes()).as_ref())
    );
    let forged_sig = format!("v1:test-key:{}\n", STANDARD.encode([0_u8; 64]));

    let mut args_web = args();
    args_web.web_only = true;

    let fixture = Fixture::new()?;
    let sigs = std::collections::BTreeMap::from([("1.2.0".to_owned(), forged_sig)]);
    let srv = super::update_support::server_with_sigs(
        Published::Good,
        None,
        vec![build("1.2.0", "0.9.0", Contents::Site)?],
        sigs,
    )
    .await?;
    let report = updater(&fixture, &srv, "0.9.0")?
        .keyring(keyring.clone())
        .run(&args_web, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains("signature verification failed"),
        "{}",
        report.stderr
    );
    assert!(!web_root(&fixture).join("current").exists());
    srv.close().await?;

    let fixture = Fixture::new()?;
    let sigs = std::collections::BTreeMap::from([("1.2.0".to_owned(), good_sig)]);
    let srv =
        super::update_support::server_with_sigs(Published::Good, None, vec![release], sigs).await?;
    let report = updater(&fixture, &srv, "0.9.0")?
        .keyring(keyring)
        .run(&args_web, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_active(&fixture, "1.2.0")?;
    srv.close().await?;
    Ok(())
}

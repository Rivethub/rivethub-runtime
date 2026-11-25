use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use cli::Exit;
use cli::service::Scope;
use cli::update::Updater;
use tokio_util::sync::CancellationToken;

use super::update_support::{
    BINARY, Fixture, Published, Result, Signer, WebRelease, args, forged, server_with_web,
    web_manifest, web_url,
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

async fn signed_run(
    releases: Vec<WebRelease>,
    sigs: &[(&str, String)],
    keyring: gateway::account::ReleaseKeyring,
) -> Result<(Fixture, cli::Report)> {
    let fixture = Fixture::new()?;
    let sigs = sigs
        .iter()
        .map(|(version, sig)| ((*version).to_owned(), sig.clone()))
        .collect();
    let server =
        super::update_support::server_with_sigs(Published::Good, None, releases, sigs).await?;
    let mut web_only = args();
    web_only.web_only = true;
    let report = updater(&fixture, &server, "0.9.0")?
        .keyring(keyring)
        .run(&web_only, &fixture.env(), &CancellationToken::new())
        .await;
    server.close().await?;
    Ok((fixture, report))
}

#[tokio::test]
async fn web_signature_fails_closed_when_forged_and_accepts_trusted_key() -> Result {
    let signer = Signer::new()?;
    for required in [false, true] {
        let release = build("1.2.0", "0.9.0", Contents::Site)?;
        let good = signer.sign(&web_manifest(&release));
        let (fixture, report) =
            signed_run(vec![release], &[("1.2.0", good)], signer.keyring(required)?).await?;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert_active(&fixture, "1.2.0")?;

        let release = build("1.2.0", "0.9.0", Contents::Site)?;
        let (fixture, report) = signed_run(
            vec![release],
            &[("1.2.0", forged())],
            signer.keyring(required)?,
        )
        .await?;
        assert_eq!(report.exit, Exit::Failure);
        assert!(
            report.stderr.contains("signature verification failed"),
            "{}",
            report.stderr
        );
        assert!(!web_root(&fixture).join("current").exists());
    }
    Ok(())
}

#[tokio::test]
async fn unsigned_web_releases_install_only_while_signatures_are_optional() -> Result {
    let signer = Signer::new()?;
    let release = build("1.2.0", "0.9.0", Contents::Site)?;
    let (fixture, report) = signed_run(vec![release], &[], signer.keyring(false)?).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_active(&fixture, "1.2.0")?;

    let release = build("1.2.0", "0.9.0", Contents::Site)?;
    let (fixture, report) = signed_run(vec![release], &[], signer.keyring(true)?).await?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains("the release does not publish it"),
        "{}",
        report.stderr
    );
    assert!(!web_root(&fixture).join("current").exists());
    Ok(())
}

#[tokio::test]
async fn bad_signatures_on_releases_that_would_not_be_selected_do_not_block_the_newest_valid_one()
-> Result {
    let signer = Signer::new()?;
    for required in [false, true] {
        let newest = build("2.0.0", "0.10.0", Contents::Site)?;
        let selected = build("1.2.0", "0.9.0", Contents::Site)?;
        let older = build("1.1.0", "0.9.0", Contents::Site)?;
        let sigs = [
            ("2.0.0", forged()),
            ("1.2.0", signer.sign(&web_manifest(&selected))),
            ("1.1.0", format!("v1:rogue-key:{}\n", "A".repeat(88))),
        ];
        let (fixture, report) = signed_run(
            vec![older, newest, selected],
            &sigs,
            signer.keyring(required)?,
        )
        .await?;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert_active(&fixture, "1.2.0")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_malformed_signature_on_the_selected_release_fails_closed_without_falling_back() -> Result
{
    let signer = Signer::new()?;
    for required in [false, true] {
        for malformed in [
            format!("v1:test-key:{}\n", "A".repeat(2048)),
            "not a signature\n".to_owned(),
        ] {
            let selected = build("1.2.0", "0.9.0", Contents::Site)?;
            let older = build("1.1.0", "0.9.0", Contents::Site)?;
            let sigs = [
                ("1.2.0", malformed),
                ("1.1.0", signer.sign(&web_manifest(&older))),
            ];
            let (fixture, report) =
                signed_run(vec![selected, older], &sigs, signer.keyring(required)?).await?;
            assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
            assert!(
                report
                    .stderr
                    .contains("web-manifest.json.sig signature verification failed for web-v1.2.0"),
                "{}",
                report.stderr
            );
            assert!(!web_root(&fixture).join("current").exists());
        }
    }
    Ok(())
}

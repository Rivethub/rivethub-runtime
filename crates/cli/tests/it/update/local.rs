use std::fs;
use std::path::{Path, PathBuf};

use cli::Exit;
use cli::service::Scope;
use cli::update::{UpdateArgs, Updater};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use super::update_support::{Fixture, Result, args};
use super::web::{Contents, assert_active, build, web_root};

const UNREACHABLE: &str = "http://127.0.0.1:9/";

struct Bundle {
    archive: PathBuf,
    manifest: PathBuf,
    sha256: String,
}

fn bundle(directory: &Path, version: &'static str, minimum: &'static str) -> Result<Bundle> {
    let release = build(version, minimum, Contents::Site)?;
    let sha256 = hex::encode(Sha256::digest(&release.archive));
    fs::create_dir_all(directory)?;
    let archive = directory.join(format!("rivethub-web-{version}.tar.gz"));
    fs::write(&archive, &release.archive)?;
    let manifest = directory.join("web-manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&crate::support::WebManifest::stable(
            version,
            minimum,
            sha256.clone(),
        ))?,
    )?;
    Ok(Bundle {
        archive,
        manifest,
        sha256,
    })
}

fn from(bundle: &Bundle) -> UpdateArgs {
    let mut args = args();
    args.web_only = true;
    args.from = Some(bundle.archive.clone());
    args.sha256 = Some(bundle.sha256.clone());
    args
}

fn offline(fixture: &Fixture, running: &str) -> Result<Updater> {
    Ok(Updater::new(
        fixture.binary.clone(),
        UNREACHABLE.parse()?,
        UNREACHABLE.parse()?,
        Scope::User,
    )
    .running(running))
}

async fn refused(fixture: &Fixture, args: &UpdateArgs, running: &str) -> Result<String> {
    let report = offline(fixture, running)?
        .run(args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert_eq!(report.stdout, "");
    assert!(!web_root(fixture).join("current").exists());
    Ok(report.stderr)
}

#[tokio::test]
async fn a_pinned_local_bundle_installs_with_no_network() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.0")?;
    let args = from(&bundle);
    let updater = offline(&fixture, "0.9.0")?;
    for message in ["Web: Installed 1.2.0.\n", "Web: 1.2.0 is already active.\n"] {
        let report = updater
            .run(&args, &fixture.env(), &CancellationToken::new())
            .await;
        assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        assert_eq!(report.stdout, message);
        assert_active(&fixture, "1.2.0")?;
    }
    assert_eq!(fs::read(&fixture.binary)?, b"old runtime");
    Ok(())
}

#[tokio::test]
async fn a_manifest_kept_elsewhere_is_read_from_its_own_path() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.0")?;
    let elsewhere = fixture.home.path().join("lock/manifest.json");
    fs::create_dir_all(fixture.home.path().join("lock"))?;
    fs::rename(&bundle.manifest, &elsewhere)?;
    let mut args = from(&bundle);
    let stderr = refused(&fixture, &args, "0.9.0").await?;
    assert!(stderr.contains("Web: Not installed."), "{stderr}");
    args.manifest = Some(elsewhere);
    let report = offline(&fixture, "0.9.0")?
        .run(&args, &fixture.env(), &CancellationToken::new())
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_active(&fixture, "1.2.0")
}

#[tokio::test]
async fn a_pin_that_differs_from_the_manifest_is_refused() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.0")?;
    let mut args = from(&bundle);
    args.sha256 = Some(hex::encode(Sha256::digest(b"another archive")));
    let stderr = refused(&fixture, &args, "0.9.0").await?;
    assert!(stderr.contains("SHA-256 verification failed"), "{stderr}");
    args.sha256 = Some("not hex".to_owned());
    let stderr = refused(&fixture, &args, "0.9.0").await?;
    assert!(stderr.contains("--sha256"), "{stderr}");
    args.sha256 = None;
    let stderr = refused(&fixture, &args, "0.9.0").await?;
    assert!(stderr.contains("--sha256"), "{stderr}");
    Ok(())
}

#[tokio::test]
async fn an_archive_that_differs_from_its_pin_is_refused() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.0")?;
    fs::write(
        &bundle.archive,
        build("1.2.0", "0.9.0", Contents::Link)?.archive,
    )?;
    let stderr = refused(&fixture, &from(&bundle), "0.9.0").await?;
    assert!(stderr.contains("SHA-256 verification failed"), "{stderr}");
    Ok(())
}

#[tokio::test]
async fn a_bundle_that_needs_a_newer_runtime_is_refused() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.1")?;
    let stderr = refused(&fixture, &from(&bundle), "0.9.0").await?;
    assert!(
        stderr.contains("web 1.2.0 needs rh 0.9.1 or newer and this is rh 0.9.0"),
        "{stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn an_explicit_static_dir_refuses_a_local_bundle() -> Result {
    let fixture = Fixture::new()?;
    let bundle = bundle(&fixture.home.path().join("downloads"), "1.2.0", "0.9.0")?;
    let report = offline(&fixture, "0.9.0")?
        .run(
            &from(&bundle),
            &fixture
                .env()
                .with_override("RIVETHUB_STATIC_DIR", fixture.home.path().join("explicit")),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains("explicit static_dir"),
        "{}",
        report.stderr
    );
    assert!(!web_root(&fixture).exists());
    Ok(())
}

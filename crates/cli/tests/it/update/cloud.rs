use std::io::Write as _;
use std::sync::Arc;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cli::Exit;
use cli::service::Scope;
use cli::update::Updater;
use gateway::account::{AccountFiles, Membership};
use parking_lot::Mutex;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use wire::gateway::account::UpdateSource;

use super::update_support::{BINARY, Fixture, Published, Result, archive, args};

const TOKEN: &str = "rht_cli-test-token";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signing {
    Good,
    Forged,
    WrongArchive,
}

struct Cloud {
    manifest: String,
    archive: Vec<u8>,
    asked: Mutex<Vec<String>>,
}

async fn cloud(signing: Signing) -> Result<(testkit::Stub, String, Arc<Cloud>)> {
    let pair = Ed25519KeyPair::generate()?;
    let key = STANDARD.encode(pair.public_key().as_ref());
    let bytes = archive(Published::Good)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let signed: [u8; 32] = match signing {
        Signing::Good | Signing::WrongArchive => digest,
        Signing::Forged => Sha256::digest(b"other").into(),
    };
    let served = match signing {
        Signing::WrongArchive => b"tampered".to_vec(),
        Signing::Good | Signing::Forged => bytes,
    };
    let state = Arc::new(Cloud {
        manifest: format!(
            r#"{{"version":"99.0.0","url":"ARCHIVE","sha256":"{}","signature":"{}","notesUrl":"https://example.com/notes"}}"#,
            hex::encode(digest),
            STANDARD.encode(pair.sign(&signed).as_ref())
        ),
        archive: served,
        asked: Mutex::new(Vec::new()),
    });
    let routes = axum::Router::new()
        .route("/v1/updates/latest", get(latest))
        .route(
            "/archive",
            get(|State(cloud): State<Arc<Cloud>>| async move { cloud.archive.clone() }),
        )
        .with_state(Arc::clone(&state));
    Ok((testkit::stub_server(routes).await?, key, state))
}

async fn latest(
    State(cloud): State<Arc<Cloud>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    cloud.asked.lock().push(format!(
        "{}?{}",
        text("authorization"),
        uri.query().unwrap_or_default()
    ));
    let body = cloud
        .manifest
        .replace("ARCHIVE", &format!("http://{}/archive", text("host")));
    ([("content-type", "application/json")], body).into_response()
}

fn configure(fixture: &Fixture, stub: &testkit::Stub, key: &str) -> Result {
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.home.path().join(".rivethub/config.yaml"))?;
    writeln!(
        config,
        "cloud:\n  url: http://{}/\n  update_key: \"{key}\"",
        stub.addr
    )?;
    Ok(())
}

fn sign_in(fixture: &Fixture) -> Result {
    let files = AccountFiles::new(&fixture.home.path().join(".rivethub"));
    let mut state = files.open()?;
    state.membership = Membership::SignedIn {
        email: "user@example.com".to_owned(),
        token: TOKEN.to_owned(),
    };
    files.save(&state)?;
    Ok(())
}

async fn update(fixture: &Fixture, source: Option<UpdateSource>) -> Result<cli::Report> {
    let mut args = args();
    args.source = source;
    let updater = Updater::new(
        fixture.binary.clone(),
        "http://127.0.0.1:9/releases/".parse()?,
        Scope::User,
    );
    Ok(tokio::time::timeout(
        std::time::Duration::from_secs(30),
        updater.run(&args, &fixture.env(), &CancellationToken::new()),
    )
    .await?)
}

#[tokio::test]
async fn a_signed_in_install_updates_from_the_signed_cloud_release() -> Result {
    let fixture = Fixture::new()?;
    let (stub, key, state) = cloud(Signing::Good).await?;
    configure(&fixture, &stub, &key)?;
    sign_in(&fixture)?;
    let report = update(&fixture, None).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report.stdout.contains("from rivethub.cloud"),
        "{}",
        report.stdout
    );
    assert!(
        report.stdout.contains("Installed v99.0.0"),
        "{}",
        report.stdout
    );
    assert_eq!(std::fs::read(&fixture.binary)?, BINARY);
    let os = gateway::account::CloudOs::current().ok_or("platform")?;
    assert_eq!(
        state.asked.lock().clone(),
        vec![format!("Bearer {TOKEN}?channel=stable&os={os}")]
    );
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_forged_signature_or_a_swapped_archive_leaves_the_install_untouched() -> Result {
    for (signing, message) in [
        (Signing::Forged, "signature does not verify"),
        (Signing::WrongArchive, "does not match the signed SHA-256"),
    ] {
        let fixture = Fixture::new()?;
        let (stub, key, _) = cloud(signing).await?;
        configure(&fixture, &stub, &key)?;
        sign_in(&fixture)?;
        let report = update(&fixture, Some(UpdateSource::RivethubCloud)).await?;
        assert_eq!(report.exit, Exit::Failure);
        assert!(report.stderr.contains(message), "{}", report.stderr);
        assert_eq!(std::fs::read(&fixture.binary)?, b"old runtime");
        assert_eq!(std::fs::read_to_string(&fixture.calls)?, "");
        stub.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn the_cloud_source_needs_a_sign_in() -> Result {
    let fixture = Fixture::new()?;
    let (stub, key, state) = cloud(Signing::Good).await?;
    configure(&fixture, &stub, &key)?;
    let report = update(&fixture, Some(UpdateSource::RivethubCloud)).await?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains("not signed in"), "{}", report.stderr);
    assert!(state.asked.lock().is_empty());
    stub.close().await?;
    Ok(())
}

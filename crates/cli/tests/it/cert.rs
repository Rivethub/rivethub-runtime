use crate::stepca_support as support;

use cli::Exit;
use rustls_pki_types::pem::PemObject;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::server::{Behavior, Exchange, Server};
use support::{Result, directory, recording, run, text, token};
use tokio_util::sync::CancellationToken;
use x509_parser::prelude::FromDer;

const PRINCIPAL: &str = "spiffe://rivet.mesh/node/node-a";
const SANS: &[&str] = &["192.0.2.15", "node-a.example.test"];

struct Fixture {
    home: tempfile::TempDir,
    server: Server,
    token: String,
    token_file: PathBuf,
    root: PathBuf,
}

#[tokio::test]
async fn enrolls_from_token_sans_then_renews_over_mtls_and_keeps_the_key() -> Result {
    let mut fixture = started(Behavior::Sign).await?;
    let recorded = recording()?;
    let report = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
    assert_eq!(
        report.exit,
        Exit::Ok,
        "{} {}",
        report.stderr,
        fixture.server.note()
    );
    assert_eq!((report.stdout.as_str(), report.stderr.as_str()), ("", ""));
    let sign = next_event(&mut fixture.server).await?;
    assert_exchange(&sign, "sign-with-token", false)?;
    let names = support::csr_names(sign.csr.as_deref().ok_or("missing csr")?)?;
    assert_eq!(
        names,
        vec!["192.0.2.15".to_owned(), "node-a.example.test".to_owned()]
    );
    let holder = fixture.home.path().join(".rivethub/tls");
    assert_eq!(mode(&holder)?, 0o700);
    assert_eq!(mode(&holder.join("holder.key.pem"))?, 0o600);
    assert_eq!(mode(&holder.join("holder.cert.pem"))?, 0o600);
    let key = std::fs::read(holder.join("holder.key.pem"))?;
    let before = std::fs::read(holder.join("holder.cert.pem"))?;
    assert!(!String::from_utf8(before.clone())?.contains(&fixture.token));
    let report = invoke(fixture.home.path(), &renew_args(&fixture, None)?).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let renewal = next_event(&mut fixture.server).await?;
    assert_exchange(&renewal, "renew-mtls", true)?;
    assert!(renewal.empty_body && renewal.csr.is_none());
    assert_eq!(key, std::fs::read(holder.join("holder.key.pem"))?);
    let after = String::from_utf8(std::fs::read(holder.join("holder.cert.pem"))?)?;
    assert_ne!(before, after.as_bytes());
    assert!(!after.contains("PRIVATE KEY") && !after.contains(&fixture.token));
    assert_eq!(lifetime(&after)?, 7 * 86_400);
    let message = recorded
        .case("sign-token-reused")?
        .message
        .clone()
        .ok_or("missing refusal")?;
    let report = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
    assert_eq!(report.exit, Exit::Failure);
    assert!(report.stderr.contains(&message));
    assert!(!report.stderr.contains(&fixture.token));
    assert_eq!(key, std::fs::read(holder.join("holder.key.pem"))?);
    assert_eq!(
        after.as_bytes(),
        &std::fs::read(holder.join("holder.cert.pem"))?
    );
    fixture.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn follows_a_same_origin_redirect_and_refuses_a_foreign_origin() -> Result {
    let mut fixture = started(Behavior::Redirect).await?;
    let report = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
    assert_eq!(
        report.exit,
        Exit::Ok,
        "{} {}",
        report.stderr,
        fixture.server.note()
    );
    let first = next_event(&mut fixture.server).await?;
    let second = next_event(&mut fixture.server).await?;
    assert_eq!(
        (first.path.as_str(), second.path.as_str()),
        ("/1.0/sign", "/1.0/issue")
    );
    assert!(!first.client_certificate && !second.client_certificate);
    let names = support::csr_names(second.csr.as_deref().ok_or("missing csr")?)?;
    assert!(!names.iter().any(|name| name.contains("spiffe")));
    fixture.server.stop().await?;
    let foreign = started(Behavior::ForeignRedirect).await?;
    let report = invoke(foreign.home.path(), &enroll_args(&foreign, None)?).await?;
    assert_eq!(report.exit, Exit::Failure, "{}", foreign.server.note());
    assert!(!report.stderr.contains(&foreign.token));
    assert!(
        !foreign
            .home
            .path()
            .join(".rivethub/tls/holder.cert.pem")
            .exists()
    );
    foreign.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn refuses_a_wrong_key_principal_or_extra_name_without_publishing() -> Result {
    let expectations = [
        (Behavior::WrongKey, "does not match the local holder key"),
        (Behavior::WrongPrincipal, "exactly the holder SPIFFE URI"),
        (Behavior::ExtraName, "was not requested"),
    ];
    for (behavior, expected) in expectations {
        let mut fixture = started(behavior).await?;
        let report = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
        assert_eq!(
            report.exit,
            Exit::Failure,
            "{behavior:?} {} {}",
            report.stderr,
            fixture.server.note()
        );
        assert!(
            report.stderr.contains(expected),
            "{behavior:?} {}",
            report.stderr
        );
        assert!(!report.stderr.contains(&fixture.token));
        assert!(
            !fixture
                .home
                .path()
                .join(".rivethub/tls/holder.cert.pem")
                .exists()
        );
        let _exchange = next_event(&mut fixture.server).await?;
        fixture.server.stop().await?;
    }
    Ok(())
}

#[tokio::test]
async fn refused_renewal_preserves_the_certificate_and_key() -> Result {
    let fixture = started(Behavior::Sign).await?;
    let report = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let holder = fixture.home.path().join(".rivethub/tls");
    let before = std::fs::read(holder.join("holder.cert.pem"))?;
    let key = std::fs::read(holder.join("holder.key.pem"))?;
    fixture.server.set_behavior(Behavior::WrongKey);
    let mismatch = invoke(fixture.home.path(), &renew_args(&fixture, None)?).await?;
    assert_eq!(mismatch.exit, Exit::Failure);
    assert!(
        mismatch
            .stderr
            .contains("does not match the local holder key")
    );
    preserved(&holder, &before, &key)?;
    fixture.server.set_behavior(Behavior::RevokedRenew);
    let message = recording()?
        .case("renew-revoked")?
        .message
        .clone()
        .ok_or("missing refusal")?;
    let revoked = invoke(fixture.home.path(), &renew_args(&fixture, None)?).await?;
    assert_eq!(revoked.exit, Exit::Failure);
    assert!(revoked.stderr.contains(&message));
    assert!(!revoked.stderr.contains(&fixture.token));
    preserved(&holder, &before, &key)?;
    fixture.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn renews_a_stored_certificate_that_is_past_its_renewal_deadline() -> Result {
    let mut fixture = started(Behavior::Overdue).await?;
    let refused = invoke(fixture.home.path(), &enroll_args(&fixture, None)?).await?;
    assert_eq!(refused.exit, Exit::Failure);
    assert!(
        refused
            .stderr
            .contains("already reached its renewal deadline")
    );
    let _sign = next_event(&mut fixture.server).await?;
    let holder = fixture.home.path().join(".rivethub/tls");
    let overdue = fixture
        .server
        .issued()
        .ok_or("missing issued certificate")?;
    let path = holder.join("holder.cert.pem");
    std::fs::write(&path, &overdue)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    fixture.server.set_behavior(Behavior::Sign);
    let report = invoke(fixture.home.path(), &renew_args(&fixture, None)?).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    let renewal = next_event(&mut fixture.server).await?;
    assert_exchange(&renewal, "renew-mtls", true)?;
    assert_ne!(overdue, std::fs::read_to_string(&path)?);
    fixture.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn long_running_renewal_obeys_cancellation_and_does_not_renew_early() -> Result {
    let mut fixture = started(Behavior::Sign).await?;
    let holder = fixture.home.path().join("holder");
    cli::cert::renew(
        renew_struct(&fixture, Some(fixture.token_file.clone()), true)?,
        holder.clone(),
        &CancellationToken::new(),
    )
    .await?;
    let sign = next_event(&mut fixture.server).await?;
    assert!(!sign.client_certificate);
    let certificate = std::fs::read_to_string(holder.join("tls/holder.cert.pem"))?;
    assert_eq!(lifetime(&certificate)?, 7 * 86_400);
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let path = holder.clone();
    let args = renew_struct(&fixture, None, false)?;
    let task = tokio::spawn(async move { cli::cert::renew(args, path, &stop).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), fixture.server.events.recv())
            .await
            .is_err()
    );
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task).await???;
    fixture.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_an_inflight_signing_request() -> Result {
    let mut fixture = started(Behavior::Wait).await?;
    let holder = fixture.home.path().join("holder");
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let path = holder.clone();
    let args = renew_struct(&fixture, Some(fixture.token_file.clone()), false)?;
    let task = tokio::spawn(async move { cli::cert::renew(args, path, &stop).await });
    let event = next_event(&mut fixture.server).await?;
    assert_eq!(event.path, "/1.0/sign");
    assert!(!event.client_certificate);
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task).await???;
    assert!(!holder.join("tls/holder.cert.pem").exists());
    fixture.server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn cancellation_before_renewal_starts_is_a_failure() -> Result {
    let home = directory()?;
    let state = home.path().join("state");
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let error = cli::cert::renew(
        cli::cert::RenewArgs {
            principal: PRINCIPAL.parse()?,
            ca_url: "https://127.0.0.1:1".parse()?,
            trust_root: home.path().join("missing.pem"),
            token_file: None,
            state: None,
            once: true,
        },
        state.clone(),
        &shutdown,
    )
    .await
    .expect_err("cancelled renewal");
    assert!(error.to_string().contains("cancelled"));
    assert!(!state.exists());
    Ok(())
}

#[tokio::test]
async fn enrolls_a_device_from_a_uri_only_token() -> Result {
    let home = directory()?;
    let principal = "spiffe://rivet.mesh/device/ada/phone";
    let mut server = Server::start(Behavior::Sign, principal).await?;
    let token_file = home.path().join("token");
    std::fs::write(&token_file, token(&[principal])?)?;
    let root = home.path().join("root.pem");
    std::fs::write(&root, &server.root_pem)?;
    let report = run(
        home.path(),
        &[
            "cert",
            "renew",
            "--principal",
            principal,
            "--ca-url",
            server.url.as_str(),
            "--trust-root",
            text(&root)?,
            "--token-file",
            text(&token_file)?,
            "--once",
        ],
    )
    .await?;
    assert_eq!(report.exit, Exit::Ok, "{} {}", report.stderr, server.note());
    let event = next_event(&mut server).await?;
    assert_eq!(
        support::csr_names(event.csr.as_deref().ok_or("missing csr")?)?,
        vec![principal.to_owned()]
    );
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn enrolls_into_a_state_directory_whose_name_contains_a_space() -> Result {
    let mut fixture = started(Behavior::Sign).await?;
    let state = fixture.home.path().join("state dir");
    let sibling = fixture.home.path().join("state dir-secret");
    std::fs::write(&sibling, b"keep")?;
    let report = invoke(fixture.home.path(), &enroll_args(&fixture, Some(&state))?).await?;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(state.join("tls/holder.cert.pem").is_file());
    assert_eq!(std::fs::read(&sibling)?, b"keep");
    let _exchange = next_event(&mut fixture.server).await?;
    fixture.server.stop().await?;
    Ok(())
}

async fn started(behavior: Behavior) -> Result<Fixture> {
    let home = directory()?;
    let server = Server::start(behavior, PRINCIPAL).await?;
    let token = token(SANS)?;
    let token_file = home.path().join("token");
    std::fs::write(&token_file, &token)?;
    let root = home.path().join("root.pem");
    std::fs::write(&root, &server.root_pem)?;
    Ok(Fixture {
        home,
        server,
        token,
        token_file,
        root,
    })
}

fn enroll_args(fixture: &Fixture, state: Option<&Path>) -> Result<Vec<String>> {
    command(fixture, Some(&fixture.token_file), state, true)
}

fn renew_args(fixture: &Fixture, state: Option<&Path>) -> Result<Vec<String>> {
    command(fixture, None, state, true)
}

fn command(
    fixture: &Fixture,
    token_file: Option<&Path>,
    state: Option<&Path>,
    once: bool,
) -> Result<Vec<String>> {
    let mut args = vec![
        "cert".to_owned(),
        "renew".to_owned(),
        "--principal".to_owned(),
        PRINCIPAL.to_owned(),
        "--ca-url".to_owned(),
        fixture.server.url.to_string(),
        "--trust-root".to_owned(),
        text(&fixture.root)?.to_owned(),
    ];
    if let Some(token_file) = token_file {
        args.push("--token-file".to_owned());
        args.push(text(token_file)?.to_owned());
    }
    if let Some(state) = state {
        args.push("--state".to_owned());
        args.push(text(state)?.to_owned());
    }
    if once {
        args.push("--once".to_owned());
    }
    Ok(args)
}

fn renew_struct(
    fixture: &Fixture,
    token_file: Option<PathBuf>,
    once: bool,
) -> Result<cli::cert::RenewArgs> {
    Ok(cli::cert::RenewArgs {
        principal: PRINCIPAL.parse()?,
        ca_url: fixture.server.url.clone(),
        trust_root: fixture.root.clone(),
        token_file,
        state: None,
        once,
    })
}

async fn invoke(home: &Path, args: &[String]) -> Result<cli::Report> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(home, &refs).await
}

async fn next_event(server: &mut Server) -> Result<Exchange> {
    tokio::time::timeout(Duration::from_secs(5), server.events.recv())
        .await?
        .ok_or_else(|| "missing step-ca exchange".into())
}

fn assert_exchange(event: &Exchange, id: &str, client: bool) -> Result {
    let recorded = recording()?;
    let case = recorded.case(id)?;
    assert_eq!(event.method, case.method);
    assert_eq!(event.path, case.path);
    assert_eq!(event.client_certificate, client);
    assert_eq!(case.client_certificate == "none", !client);
    Ok(())
}

fn preserved(holder: &Path, certificate: &[u8], key: &[u8]) -> Result {
    assert_eq!(certificate, std::fs::read(holder.join("holder.cert.pem"))?);
    assert_eq!(key, std::fs::read(holder.join("holder.key.pem"))?);
    Ok(())
}

fn mode(path: &Path) -> Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

fn lifetime(pem: &str) -> Result<i64> {
    let der = rustls_pki_types::CertificateDer::from_pem_slice(pem.as_bytes())?;
    let (_, certificate) = x509_parser::certificate::X509Certificate::from_der(&der)?;
    Ok(
        certificate.validity().not_after.timestamp()
            - certificate.validity().not_before.timestamp(),
    )
}

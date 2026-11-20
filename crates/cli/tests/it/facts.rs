use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cli::Exit;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use testkit::FakeMemoryStore;
use wire::gateway::auth::SessionRequest;

use crate::capture_hook::{local_gateway, local_gateway_with};
use crate::support::{self, WebManifest, config, environment, home, run};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Deserialize)]
pub(crate) struct Status {
    pub(crate) running: bool,
    pub(crate) version: Option<String>,
    pub(crate) cli_version: String,
    pub(crate) origin: String,
    pub(crate) health_error: Option<String>,
    pub(crate) web: Web,
    pub(crate) tls: Option<Tls>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Web {
    installed: bool,
    source: String,
    path: Option<PathBuf>,
    version: Option<String>,
    min_rh_version: Option<String>,
    compatible: Option<bool>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Tls {
    pub(crate) cert: PathBuf,
    pub(crate) ca: Option<PathBuf>,
    pub(crate) sha256: String,
    pub(crate) spki_sha256: String,
    pub(crate) not_after: jiff::Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum Opened {
    Minted {
        origin: String,
        url: String,
        code: String,
        expires_at: jiff::Timestamp,
    },
    Failed {
        reason: String,
        error: String,
    },
}

fn listening_on(home: &tempfile::TempDir, port: u16) {
    config(
        home,
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    );
}

fn install_web(home: &tempfile::TempDir, version: &str, minimum: &str) -> TestResult {
    let root = home.path().join(".rivethub/web");
    let bundle = root.join(version);
    std::fs::create_dir_all(&bundle)?;
    std::fs::write(bundle.join("index.html"), "<!doctype html>app")?;
    std::fs::write(
        bundle.join("web-manifest.json"),
        serde_json::to_vec(&WebManifest::stable(version, minimum, "0".repeat(64)))?,
    )?;
    let current = root.join("current");
    if current.symlink_metadata().is_ok() {
        std::fs::remove_file(&current)?;
    }
    symlink(version, current)?;
    Ok(())
}

pub(crate) async fn document<T: DeserializeOwned>(
    args: &[&str],
    env: &config::Environment,
) -> TestResult<(Exit, T)> {
    let report = run(args, env).await;
    Ok((report.exit, serde_json::from_str(&report.stdout)?))
}

async fn unused_port() -> TestResult<u16> {
    Ok(tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await?
        .local_addr()?
        .port())
}

#[tokio::test]
async fn status_json_describes_a_running_node_and_the_web_app_it_can_serve() -> TestResult {
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    let store = Arc::new(FakeMemoryStore::default());
    let (port, stop, server) = local_gateway(&store, home.path(), owner).await?;
    listening_on(&home, port);

    let (exit, status) = document::<Status>(&["status", "--json"], &env).await?;
    assert_eq!(exit, Exit::Ok);
    assert!(status.running);
    assert_eq!(status.health_error, None);
    assert_eq!(status.origin, format!("http://127.0.0.1:{port}"));
    assert_eq!(status.cli_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.tls, None);
    assert_eq!(
        status.web,
        Web {
            installed: false,
            source: "none".to_owned(),
            path: None,
            version: None,
            min_rh_version: None,
            compatible: None,
        }
    );

    install_web(&home, "1.2.0", "0.1.0")?;
    let (_, status) = document::<Status>(&["status", "--json"], &env).await?;
    assert_eq!(
        status.web,
        Web {
            installed: true,
            source: "managed".to_owned(),
            path: Some(home.path().join(".rivethub/web/current")),
            version: Some("1.2.0".to_owned()),
            min_rh_version: Some("0.1.0".to_owned()),
            compatible: Some(true),
        }
    );

    install_web(&home, "1.3.0", "999.0.0")?;
    let (_, status) = document::<Status>(&["status", "--json"], &env).await?;
    assert_eq!(status.web.version.as_deref(), Some("1.3.0"));
    assert_eq!(status.web.compatible, Some(false));

    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

#[tokio::test]
async fn status_json_names_the_origin_of_a_node_that_is_not_running() -> TestResult {
    let home = home();
    let port = unused_port().await?;
    listening_on(&home, port);
    let explicit = home.path().join("site");
    let env = environment(&home).with_override("RIVETHUB_STATIC_DIR", &explicit);

    let (exit, status) = document::<Status>(&["status", "--json"], &env).await?;
    assert_eq!(exit, Exit::Ok);
    assert!(!status.running);
    assert!(status.health_error.is_some());
    assert_eq!(status.version, None);
    assert_eq!(status.origin, format!("http://127.0.0.1:{port}"));
    assert_eq!(status.web.source, "explicit");
    assert!(!status.web.installed);

    std::fs::create_dir_all(&explicit)?;
    std::fs::write(explicit.join("index.html"), "<!doctype html>app")?;
    let (_, status) = document::<Status>(&["status", "--json"], &env).await?;
    assert!(status.web.installed);
    assert_eq!(status.web.path, Some(explicit));
    Ok(())
}

#[tokio::test]
async fn open_json_prints_a_code_that_signs_in_and_when_it_expires() -> TestResult {
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    let store = Arc::new(FakeMemoryStore::default());
    let site = tempfile::tempdir()?;
    std::fs::write(site.path().join("index.html"), "<!doctype html>app")?;
    let (port, stop, server) = local_gateway_with(
        &store,
        home.path(),
        Arc::clone(&owner),
        Some(site.path().to_path_buf()),
    )
    .await?;
    listening_on(&home, port);

    let before = jiff::Timestamp::now();
    let report = run(&["open", "--json"], &env).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stderr, "");
    assert!(!report.stdout.contains(owner.current().expose()));
    let Opened::Minted {
        origin,
        url,
        code,
        expires_at,
    } = serde_json::from_str(&report.stdout)?
    else {
        return Err("rh open --json did not mint a code".into());
    };
    assert_eq!(origin, format!("http://127.0.0.1:{port}"));
    assert_eq!(url, format!("{origin}/login#code={code}"));
    assert!(expires_at > before, "{expires_at} is not after {before}");

    let signed_in = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .post(format!("{origin}/api/auth/session"))
        .json(&SessionRequest {
            code: wire::gateway::auth::Secret::new(code),
        })
        .send()
        .await?;
    assert_eq!(signed_in.status(), 200);

    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    Ok(())
}

async fn refusal(env: &config::Environment) -> TestResult<(Exit, String)> {
    match document::<Opened>(&["open", "--json"], env).await? {
        (exit, Opened::Failed { reason, error }) if !error.is_empty() => Ok((exit, reason)),
        other => Err(format!("rh open --json did not fail with a reason: {other:?}").into()),
    }
}

#[tokio::test]
async fn open_json_tells_a_missing_web_app_from_a_stopped_node() -> TestResult {
    assert!(format!("{:?}", Exit::NoWebApp.code()).contains("12"));
    let home = home();
    let env = environment(&home);
    let owner = support::local_owner(home.path());
    let store = Arc::new(FakeMemoryStore::default());
    let (port, stop, server) = local_gateway(&store, home.path(), owner).await?;
    listening_on(&home, port);

    assert_eq!(
        refusal(&env).await?,
        (Exit::NoWebApp, "no_web_app".to_owned())
    );

    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    assert_eq!(
        refusal(&env).await?,
        (Exit::Failure, "not_running".to_owned())
    );
    Ok(())
}

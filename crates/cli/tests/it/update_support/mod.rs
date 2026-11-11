#![cfg(test)]

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Redirect, Response};
use axum::routing::get;
use cli::service::Scope;
use cli::update::{Channel, UpdateArgs, Updater};
use config::Environment;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub(crate) const BINARY: &[u8] = b"#!/bin/sh\nprintf 'new binary\\n'\n";
pub(crate) const MIGRATION: &[u8] = br#"#!/bin/sh
printf 'migrate' >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
if [ "$RIVETHUB_TEST_FAIL" = migration ]; then
    printf 'migration failed\n' >&2
    exit 1
fi
printf 'schema ready\n'
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    Good,
    BadChecksum,
    MissingMigration,
}

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) struct Fixture {
    pub(crate) home: TempDir,
    pub(crate) binary: PathBuf,
    pub(crate) bin: PathBuf,
    pub(crate) calls: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self> {
        let home = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
        let install = home.path().join("install 'quoted' $(literal)");
        std::fs::create_dir(&install)?;
        let binary = install.join("rh");
        std::fs::write(&binary, "old runtime")?;
        std::fs::write(install.join("rivethub-migrate"), "old migration")?;
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin)?;
        let state = home.path().join(".rivethub");
        std::fs::create_dir(&state)?;
        let database = home.path().join("memory.sqlite");
        std::fs::write(
            state.join("config.yaml"),
            format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\nmesh:\n  node_name: local\n",
                database.display()
            ),
        )?;
        let unit = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit)?;
        std::fs::write(
            unit.join("rivethub.service"),
            "[Service]\nExecStart=rh start\n",
        )?;
        let calls = home.path().join("calls");
        std::fs::write(&calls, "")?;
        let fixture = Self {
            home,
            binary,
            bin,
            calls,
        };
        fixture.script(
            "systemctl",
            r#"#!/bin/sh
printf 'systemctl' >> "$RIVETHUB_TEST_CALLS"
for arg do printf '|%s' "$arg" >> "$RIVETHUB_TEST_CALLS"; done
printf '\n' >> "$RIVETHUB_TEST_CALLS"
if [ "$2" = "$RIVETHUB_TEST_FAIL" ]; then exit 1; fi
"#,
        )?;
        fixture.script(
            "ssh",
            r#"#!/bin/sh
call=ssh
for arg do call="$call|$arg"; done
printf '%s\n' "$call" >> "$RIVETHUB_TEST_CALLS"
for arg do
    case "$arg" in
        unreachable.example.com) exit 255 ;;
        failed.example.com) failed=yes ;;
    esac
    last="$arg"
done
if [ "$failed" = yes ] && [ "$last" != true ]; then exit 1; fi
"#,
        )?;
        Ok(fixture)
    }

    pub(crate) fn script(&self, name: &str, script: &str) -> Result {
        let path = self.bin.join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    pub(crate) fn env(&self) -> Environment {
        Environment::from_vars([
            (OsStr::new("HOME"), self.home.path().as_os_str()),
            (OsStr::new("PATH"), self.bin.as_os_str()),
            (OsStr::new("RIVETHUB_TEST_CALLS"), self.calls.as_os_str()),
            (
                OsStr::new("RIVETHUB_SHARED_DIR"),
                self.home.path().as_os_str(),
            ),
        ])
    }

    pub(crate) async fn run(
        &self,
        args: &UpdateArgs,
        env: &Environment,
        published: Published,
    ) -> Result<cli::Report> {
        let server = server(published).await?;
        let updater = Updater::new(
            self.binary.clone(),
            format!("http://{}/releases/", server.addr).parse()?,
            Scope::User,
        );
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            updater.run(args, env, &CancellationToken::new()),
        )
        .await?;
        server.close().await?;
        Ok(report)
    }
}

pub(crate) fn args() -> UpdateArgs {
    UpdateArgs {
        channel: Channel::Stable,
        version: None,
        no_restart: false,
        source: None,
    }
}

#[derive(Serialize)]
struct Release {
    tag_name: &'static str,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Serialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

const RELEASES: [(&str, bool, bool); 3] = [
    ("v0.9.0", false, false),
    ("v0.11.0", true, true),
    ("v0.10.0", false, true),
];

fn archive_name(tag: &str) -> String {
    format!(
        "rh-{}-{}-unknown-linux-gnu.tar.gz",
        tag.trim_start_matches('v'),
        std::env::consts::ARCH
    )
}

pub(crate) fn archive(published: Published) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut entries = vec![
        ("LICENSE", b"license".as_slice()),
        ("rh", BINARY),
        ("rivethub-migrate", MIGRATION),
    ];
    if published == Published::MissingMigration {
        entries.pop();
    }
    for (name, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(contents.len())?);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, contents)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

struct Assets {
    manifest: String,
    archive: Vec<u8>,
}

pub(crate) async fn server(published: Published) -> Result<testkit::Stub> {
    let archive = archive(published)?;
    let digest = hex::encode(Sha256::digest(if published == Published::BadChecksum {
        b"wrong".as_slice()
    } else {
        &archive
    }));
    let mut manifest = String::new();
    for (release_tag, _, _) in RELEASES {
        writeln!(manifest, "{digest}  {}", archive_name(release_tag))?;
    }
    let routes = axum::Router::new()
        .route("/releases", get(list))
        .route("/releases/latest", get(latest))
        .route("/releases/tags/{tag}", get(tagged))
        .route("/assets/{name}", get(download))
        .with_state(Arc::new(Assets { manifest, archive }));
    Ok(testkit::stub_server(routes).await?)
}

fn release(
    headers: &HeaderMap,
    (tag_name, draft, prerelease): (&'static str, bool, bool),
) -> Release {
    let host = headers
        .get("host")
        .expect("host")
        .to_str()
        .expect("host text");
    Release {
        tag_name,
        draft,
        prerelease,
        assets: [archive_name(tag_name), "SHA256SUMS".to_owned()]
            .into_iter()
            .map(|name| Asset {
                browser_download_url: format!("http://{host}/assets/{name}"),
                name,
            })
            .collect(),
    }
}

async fn list(headers: HeaderMap) -> Response {
    let releases: Vec<_> = RELEASES
        .into_iter()
        .map(|entry| release(&headers, entry))
        .collect();
    axum::Json(releases).into_response()
}

async fn latest(headers: HeaderMap) -> Response {
    axum::Json(release(&headers, RELEASES[0])).into_response()
}

async fn tagged(Path(tag): Path<String>, headers: HeaderMap) -> Response {
    match RELEASES.into_iter().find(|(name, _, _)| *name == tag) {
        Some(entry) => axum::Json(release(&headers, entry)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn download(Path(name): Path<String>, State(assets): State<Arc<Assets>>) -> Response {
    if name == "SHA256SUMS" {
        assets.manifest.clone().into_response()
    } else if RELEASES
        .into_iter()
        .any(|(tag, _, _)| name == archive_name(tag))
    {
        Redirect::temporary("/assets/archive").into_response()
    } else if name == "archive" {
        assets.archive.clone().into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

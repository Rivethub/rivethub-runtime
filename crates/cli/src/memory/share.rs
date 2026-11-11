use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use tokio::io::AsyncReadExt as _;

use capture::seal::CodeError;
use capture::seal::MAX_BUNDLE_BYTES;
use capture::seal::OpenError;
use capture::seal::SealError;
use capture::seal::ShareCode;
use capture::sign::KeyParseError;
use capture::sign::PublicKey;
use capture::sign::SignError;
use capture::sign::TrustError;
use capture::sign::TrustStore;
use capture::sign::Verification;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use thiserror::Error;
use wire::capture::Receipt;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::desk::Shared;
use super::dropbox::DropBox;
use super::dropbox::MAX_DROP_BYTES;
use super::inbox::Inbox;
use super::inbox::Slot;
use super::parse::MemoryCommand;
use super::parse::reject;
use crate::Error;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;
use crate::share_server::ShareServers;

#[derive(Debug, Error)]
pub enum ShareError {
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error(transparent)]
    Code(#[from] CodeError),
    #[error(transparent)]
    Sign(#[from] SignError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Key(#[from] KeyParseError),
    #[error("{0} is not a file; give the sanitized bundle rh memory sanitize wrote")]
    NotFile(PathBuf),
    #[error(
        "the bundle is raw; a shared bundle must be sanitized by its sender, so nothing was imported"
    )]
    Raw,
    #[error(
        "the bundle claims to be sanitized, but {count} secret pattern match(es) remain ({first}); nothing was imported and the ciphertext stays in the inbox"
    )]
    Secrets { count: usize, first: String },
    #[error(
        "the bundle still withholds {0} flagged chunks; nothing was imported and the ciphertext stays in the inbox"
    )]
    Withheld(usize),
    #[error("{0}")]
    Usage(&'static str),
}

#[derive(Debug)]
pub(super) enum ShareCommand {
    Share {
        path: PathBuf,
        server: Option<String>,
        signed: bool,
        flags: usize,
    },
    Receive {
        code: ShareCode,
        dry_run: bool,
    },
    KeyShow,
    TrustAdd {
        name: String,
        key: PublicKey,
    },
    TrustList,
    TrustRemove {
        name: String,
        key: Option<PublicKey>,
    },
}

const SHARE_USAGE: &str =
    "usage: rh memory share <sanitized bundle> [--server <host>] [--unsigned] [--flags <n>]";
const RECEIVE_USAGE: &str = "usage: rh memory receive <share code> [--dry-run]";
const KEY_USAGE: &str = "usage: rh memory key show";
const TRUST_USAGE: &str =
    "usage: rh memory trust add <name> <key> | trust list | trust remove <name> [<key>]";

fn usage(text: &'static str) -> Error {
    ShareError::Usage(text).into()
}

pub(super) fn share_args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut path = None;
    let mut server = None;
    let mut signed = true;
    let mut flags = 0;
    while let Some(token) = scan.current().map(str::to_owned) {
        if scan.flag("--unsigned") {
            signed = false;
            continue;
        }
        match scan.option("--server", None) {
            Taken::Value(value) if !value.trim().is_empty() => {
                server = Some(value);
                continue;
            }
            Taken::Value(_) | Taken::Missing => return Err(usage(SHARE_USAGE)),
            Taken::Absent => {}
        }
        match scan.option("--flags", None) {
            Taken::Value(value) => {
                flags = value.parse().map_err(|_| usage(SHARE_USAGE))?;
                continue;
            }
            Taken::Missing => return Err(usage(SHARE_USAGE)),
            Taken::Absent => {}
        }
        if token.starts_with('-') || path.is_some() {
            return Err(reject(&scan));
        }
        path = Some(PathBuf::from(token));
        scan.bump();
    }
    let path = path.ok_or_else(|| usage(SHARE_USAGE))?;
    Ok(MemoryCommand::Share(ShareCommand::Share {
        path,
        server,
        signed,
        flags,
    }))
}

pub(super) fn receive_args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut code = None;
    let mut dry_run = false;
    while let Some(token) = scan.current().map(str::to_owned) {
        if scan.flag("--dry-run") {
            dry_run = true;
            continue;
        }
        if token.starts_with('-') || code.is_some() {
            return Err(reject(&scan));
        }
        code = Some(token.parse::<ShareCode>().map_err(ShareError::from)?);
        scan.bump();
    }
    let code = code.ok_or_else(|| usage(RECEIVE_USAGE))?;
    Ok(MemoryCommand::Share(ShareCommand::Receive {
        code,
        dry_run,
    }))
}

pub(super) fn key_args(args: &[String]) -> Result<MemoryCommand, Error> {
    match args {
        [show] if show == "show" => Ok(MemoryCommand::Share(ShareCommand::KeyShow)),
        _ => Err(usage(KEY_USAGE)),
    }
}

pub(super) fn trust_args(args: &[String]) -> Result<MemoryCommand, Error> {
    let key = |text: &String| text.parse::<PublicKey>().map_err(ShareError::from);
    let command = match args {
        [verb, name, text] if verb == "add" => ShareCommand::TrustAdd {
            name: name.clone(),
            key: key(text)?,
        },
        [verb] if verb == "list" => ShareCommand::TrustList,
        [verb, name] if verb == "remove" => ShareCommand::TrustRemove {
            name: name.clone(),
            key: None,
        },
        [verb, name, text] if verb == "remove" => ShareCommand::TrustRemove {
            name: name.clone(),
            key: Some(key(text)?),
        },
        _ => return Err(usage(TRUST_USAGE)),
    };
    Ok(MemoryCommand::Share(command))
}

pub(super) async fn run(command: ShareCommand, env: &Environment) -> Result<Report, Error> {
    match command {
        ShareCommand::Share {
            path,
            server,
            signed,
            flags,
        } => {
            let text = read_bundle_file(&path).await?;
            let sealed = super::desk::seal_bytes(env, text, signed, flags).await?;
            let shared = super::desk::upload_to(env, server.as_deref(), sealed).await?;
            let kept = tokio::fs::remove_file(&path).await.err().map(|error| {
                format!(
                    "warning: the bundle was shared but {} could not be removed ({error}); delete it yourself\n",
                    path.display()
                )
            });
            Ok(share_report(&shared, kept.as_deref()))
        }
        ShareCommand::Receive { code, dry_run } => receive(env, code, dry_run).await,
        ShareCommand::KeyShow => key_show(env),
        ShareCommand::TrustAdd { name, key } => {
            let path = trust_path(env)?;
            let mut trust = TrustStore::load(&path).map_err(ShareError::from)?;
            trust.add(&name, key).map_err(ShareError::from)?;
            trust.save(&path).map_err(ShareError::from)?;
            Ok(Report::success(format!(
                "trusted {key} ({}) as {}; bundles {name} signs with it now import as verified\n",
                key.fingerprint(),
                name.trim()
            )))
        }
        ShareCommand::TrustList => trust_list(env),
        ShareCommand::TrustRemove { name, key } => {
            let path = trust_path(env)?;
            let mut trust = TrustStore::load(&path).map_err(ShareError::from)?;
            let removed = trust.remove(&name, key.as_ref());
            trust.save(&path).map_err(ShareError::from)?;
            Ok(Report::success(format!(
                "removed {removed} trusted key(s) for {name}\n"
            )))
        }
    }
}

fn trust_path(env: &Environment) -> Result<PathBuf, Error> {
    Ok(TrustStore::path_in(&capture::share::config_home(env)?))
}

fn key_show(env: &Environment) -> Result<Report, Error> {
    let key = super::desk::node_key(env)?;
    Ok(Report::success(format!(
        "{key}\nfingerprint {}\nsend the key line to the people you share with; they run rh memory trust add <your name> <key> after checking the fingerprint with you\n",
        key.fingerprint()
    )))
}

fn trust_list(env: &Environment) -> Result<Report, Error> {
    let trust = TrustStore::load(&trust_path(env)?).map_err(ShareError::from)?;
    if trust.senders().is_empty() {
        return Ok(Report::success(
            "no trusted senders; rh memory trust add <name> <key> adds one\n".to_owned(),
        ));
    }
    Ok(Report::success(trust.senders().iter().fold(
        String::new(),
        |mut lines, sender| {
            lines.push_str(&sender.name);
            lines.push('\t');
            lines.push_str(&sender.key.to_string());
            lines.push('\t');
            lines.push_str(&sender.key.fingerprint());
            lines.push('\n');
            lines
        },
    )))
}

pub async fn read_bundle_file(path: &Path) -> Result<Vec<u8>, Error> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.is_file() {
        return Err(ShareError::NotFile(path.to_path_buf()).into());
    }
    let mut text = Vec::new();
    tokio::fs::File::open(path)
        .await?
        .take(MAX_BUNDLE_BYTES as u64 + 1)
        .read_to_end(&mut text)
        .await?;
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(ShareError::Seal(SealError::TooLarge {
            bytes: metadata.len().max(text.len() as u64),
        })
        .into());
    }
    Ok(text)
}

#[must_use]
pub fn code_line(shared: &Shared) -> String {
    format!(
        "share code (works once, expires {}; send it privately): {}",
        shared.expires_at.format("%Y-%m-%d"),
        shared.code
    )
}

fn share_report(shared: &Shared, kept: Option<&str>) -> Report {
    let signed = shared.signed.map_or_else(
        || "unsigned".to_owned(),
        |key| format!("signed {}", key.fingerprint()),
    );
    let mut report = Report::success(format!(
        "sealed {} ({signed}), uploaded {} KiB; {}\n",
        sessions_noun(shared.sessions),
        shared.uploaded.div_ceil(1024),
        code_line(shared)
    ));
    if let Some(kept) = kept {
        report.stderr.push_str(kept);
    }
    report
}

fn sessions_noun(count: usize) -> String {
    if count == 1 {
        "1 session".to_owned()
    } else {
        format!("{count} sessions")
    }
}

async fn receive(env: &Environment, code: ShareCode, dry_run: bool) -> Result<Report, Error> {
    let inbox = Inbox::in_state(&capture::share::state_home(env)?);
    inbox.prune(SystemTime::now())?;
    let id = code.id.clone();
    let slot = Slot::of(&code.server, &id);
    if inbox.was_received(&slot) {
        return Ok(Report::success(format!(
            "bundle {id} was already received on this machine; nothing was downloaded\n"
        )));
    }
    let ciphertext = if let Some(kept) = inbox.read(&slot, MAX_DROP_BYTES)? {
        kept
    } else {
        let servers = ShareServers::from_env(env).await?;
        let drops = DropBox::on(&servers, &code.server).await?;
        writeln!(std::io::stderr(), "downloading from {}", code.server)?;
        let partial = inbox.partial()?;
        drops.download(&id, partial.as_file().try_clone()?).await?;
        inbox.keep(&slot, partial)?;
        inbox
            .read(&slot, MAX_DROP_BYTES)?
            .ok_or_else(|| std::io::Error::other("the inbox lost the download"))?
    };
    let trust = TrustStore::load(&trust_path(env)?).map_err(ShareError::from)?;
    let opened =
        tokio::task::spawn_blocking(move || capture::seal::open(ciphertext, &code.key, &trust))
            .await
            .map_err(std::io::Error::other)?
            .map_err(ShareError::from)?;
    screen(&opened.bundle)?;
    let user = opened.bundle.manifest.exported_by.user.clone();
    let reason = opened.verification.reason(&user);
    let sessions = sessions_noun(opened.bundle.sessions.len());
    if dry_run {
        return Ok(Report::success(format!(
            "would import {sessions} from {user} ({reason}); nothing written, and the ciphertext stays in {}\n",
            inbox.ciphertext_path(&slot).display()
        )));
    }
    let now = Utc::now();
    let receipt = receipt(&id, now, &opened.verification);
    let totals =
        crate::capture_hook::ingest_bundle(env, &opened.bundle, Some(&receipt), now).await?;
    inbox.mark_received(&slot)?;
    Ok(Report::success(format!(
        "received {sessions} from {user} ({reason}); imported {}: {} messages inserted, {} skipped\n",
        opened.bundle.sessions.len(),
        totals.inserted,
        totals.skipped
    )))
}

fn receipt(id: &capture::seal::DropId, at: DateTime<Utc>, verification: &Verification) -> Receipt {
    Receipt {
        bundle_id: id.to_string(),
        received_at: at,
        verification: verification.kind(),
        key_fingerprint: verification.key().map(PublicKey::fingerprint),
    }
}

fn screen(bundle: &SessionBundle) -> Result<(), ShareError> {
    if bundle.manifest.sanitization == Sanitization::Raw {
        return Err(ShareError::Raw);
    }
    let secrets = capture::sanitize::secrets(bundle);
    if let Some(first) = secrets.first() {
        return Err(ShareError::Secrets {
            count: secrets.len(),
            first: first.to_string(),
        });
    }
    let flags = capture::sanitize::open_flags(bundle).map_err(SealError::from)?;
    if flags.withheld > 0 {
        return Err(ShareError::Withheld(flags.withheld));
    }
    Ok(())
}

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use config::Environment;
use jiff::Timestamp;
use kernel::cleanup::BackfillBatch;
use kernel::tidy::TidyStore as _;
use memory_postgres::{DatabaseIdentity, PostgresMemory};
use serde::Deserialize;
use serde::Serialize;

use super::Stage;
use super::filing::Filed;
use super::fold::Folds;
use crate::Error;

const PROPOSED: &str = "proposed.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Proposed {
    batch: String,
    proposals: usize,
    database: String,
    #[serde(default)]
    identity: Option<super::stamp::TargetStamp>,
}

pub(super) async fn guard_live(
    mode: super::Mode,
    backend: &super::Backend,
    env: &Environment,
) -> Result<(), Error> {
    match (mode, backend) {
        (super::Mode::DryRun, _) => Ok(()),
        (super::Mode::Propose, super::Backend::Postgres(url)) => {
            refuse_live(url, env).await?;
            Ok(())
        }
        (super::Mode::Propose, super::Backend::Sqlite(path)) => refuse_sqlite(path, env),
    }
}

pub(super) struct Proposal<'a> {
    pub(super) mode: &'a super::Mode,
    pub(super) backend: &'a super::Backend,
    pub(super) env: &'a Environment,
    pub(super) stamp: &'a super::stamp::TargetStamp,
}

pub(super) async fn commit(
    stage: &Stage<'_>,
    filed: &Filed,
    folds: &Folds,
    proposal: &Proposal<'_>,
) -> Result<String, Error> {
    match (proposal.mode, proposal.backend) {
        (super::Mode::DryRun, _) => Ok(String::new()),
        (super::Mode::Propose, super::Backend::Postgres(url)) => {
            let live = refuse_live(url, proposal.env).await?;
            write(stage, filed, folds, url, &live, proposal.stamp).await
        }
        (super::Mode::Propose, super::Backend::Sqlite(path)) => {
            write_sqlite(stage, filed, folds, path, proposal.env, proposal.stamp).await
        }
    }
}

pub(super) async fn refuse_foreign_receipt(
    book: &super::Book,
    stamp: &super::stamp::TargetStamp,
) -> Result<(), Error> {
    match book.read::<Proposed>(PROPOSED).await? {
        Some(done) => receipt_matches(&done, stamp),
        None => Ok(()),
    }
}

fn receipt_matches(done: &Proposed, stamp: &super::stamp::TargetStamp) -> Result<(), Error> {
    match &done.identity {
        Some(found) if found == stamp => Ok(()),
        Some(_) => Err(Error::CleanupOutMismatch),
        None => Err(Error::CleanupOutUnstamped),
    }
}

fn refuse_sqlite(target: &Path, env: &Environment) -> Result<(), Error> {
    let target_id = file_id(target)?.ok_or(Error::CleanupTargetMissing)?;
    let Some(live) = super::super::target::configured_sqlite(env)? else {
        return Err(Error::CleanupLiveUnknown);
    };
    let live_id = file_id(&live)?.ok_or(Error::CleanupLiveMissing)?;
    if target_id == live_id {
        Err(Error::CleanupLive)
    } else {
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct FileId {
    pub(super) device: u64,
    pub(super) inode: u64,
}

pub(super) fn file_id(path: &Path) -> Result<Option<FileId>, Error> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(FileId {
            device: meta.dev(),
            inode: meta.ino(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn write_sqlite(
    stage: &Stage<'_>,
    filed: &Filed,
    folds: &Folds,
    target: &Path,
    env: &Environment,
    stamp: &super::stamp::TargetStamp,
) -> Result<String, Error> {
    if let Some(done) = stage.book.read::<Proposed>(PROPOSED).await? {
        receipt_matches(&done, stamp)?;
        return Ok(format!(
            "batch {} with {} proposals was already written to {}\n",
            done.batch, done.proposals, done.database
        ));
    }
    refuse_sqlite(target, env)?;
    let memory = super::super::run::open_sqlite(target).await?;
    let opened = file_id(target)?.ok_or(Error::CleanupTargetMissing)?;
    if !super::stamp::matches_file(stamp, opened.device, opened.inode) {
        return Err(Error::CleanupOutMismatch);
    }
    let batch = BackfillBatch {
        snapshot: stage.snapshot,
        tree: &filed.tree,
        filings: &filed.filings,
        pages: &folds.pages,
        generic: &filed.generic,
    }
    .batch(Timestamp::now())?;
    let stored = memory.propose(batch).await?;
    let proposed = Proposed {
        batch: stored.id.to_string(),
        proposals: stored.proposals.len(),
        database: target.display().to_string(),
        identity: Some(stamp.clone()),
    };
    stage.book.write(PROPOSED, &proposed).await?;
    Ok(format!(
        "proposed batch {} with {} proposals in {}\n",
        proposed.batch, proposed.proposals, proposed.database
    ))
}

pub(super) async fn refuse_live(
    target: &str,
    env: &Environment,
) -> Result<DatabaseIdentity, Error> {
    let target_identity = identity_at(target).await?;
    let live_url = match super::super::target::load_present(env)? {
        None => env.postgres_url(),
        Some(_) => Some(super::super::run::postgres_url(env)?),
    };
    let Some(live_url) = live_url else {
        return Err(Error::PostgresUrlRequired);
    };
    let live = identity_at(&live_url).await?;
    if target_identity == live {
        return Err(Error::CleanupLive);
    }
    Ok(live)
}

fn refuse_write(
    identity: &DatabaseIdentity,
    live: &DatabaseIdentity,
    stamp: &super::stamp::TargetStamp,
) -> Option<Error> {
    if identity == live {
        Some(Error::CleanupLive)
    } else if super::stamp::matches_postgres(stamp, identity) {
        None
    } else {
        Some(Error::CleanupOutMismatch)
    }
}

async fn identity_at(url: &str) -> Result<DatabaseIdentity, Error> {
    let memory = PostgresMemory::connect_read_only(url).await?;
    let identity = memory.identity().await;
    memory.pool().close().await;
    Ok(identity?)
}

pub(super) async fn write(
    stage: &Stage<'_>,
    filed: &Filed,
    folds: &Folds,
    target: &str,
    live: &DatabaseIdentity,
    stamp: &super::stamp::TargetStamp,
) -> Result<String, Error> {
    if let Some(done) = stage.book.read::<Proposed>(PROPOSED).await? {
        receipt_matches(&done, stamp)?;
        return Ok(format!(
            "batch {} with {} proposals was already written to {}\n",
            done.batch, done.proposals, done.database
        ));
    }
    let memory = PostgresMemory::connect(target).await?;
    let identity = memory.identity().await?;
    if let Some(error) = refuse_write(&identity, live, stamp) {
        memory.pool().close().await;
        return Err(error);
    }
    let batch = BackfillBatch {
        snapshot: stage.snapshot,
        tree: &filed.tree,
        filings: &filed.filings,
        pages: &folds.pages,
        generic: &filed.generic,
    }
    .batch(Timestamp::now())?;
    let stored = memory.propose(batch).await;
    memory.pool().close().await;
    let stored = stored?;
    let proposed = Proposed {
        batch: stored.id.to_string(),
        proposals: stored.proposals.len(),
        database: identity.name,
        identity: Some(stamp.clone()),
    };
    stage.book.write(PROPOSED, &proposed).await?;
    Ok(format!(
        "proposed batch {} with {} proposals in {}\n",
        proposed.batch, proposed.proposals, proposed.database
    ))
}

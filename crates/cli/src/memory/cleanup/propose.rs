use chrono::Utc;
use config::Environment;
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
}

pub(super) async fn refuse_live(
    target: &str,
    env: &Environment,
) -> Result<DatabaseIdentity, Error> {
    let live = super::super::run::postgres_url(env)?;
    let live = identity_at(&live).await?;
    if identity_at(target).await? == live {
        return Err(Error::CleanupLive);
    }
    Ok(live)
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
) -> Result<String, Error> {
    if let Some(done) = stage.book.read::<Proposed>(PROPOSED).await? {
        return Ok(format!(
            "batch {} with {} proposals was already written to {}\n",
            done.batch, done.proposals, done.database
        ));
    }
    let memory = PostgresMemory::connect(target).await?;
    let identity = memory.identity().await?;
    if identity == *live {
        memory.pool().close().await;
        return Err(Error::CleanupLive);
    }
    let batch = BackfillBatch {
        snapshot: stage.snapshot,
        tree: &filed.tree,
        filings: &filed.filings,
        pages: &folds.pages,
        generic: &filed.generic,
    }
    .batch(Utc::now())?;
    let stored = memory.propose(batch).await;
    memory.pool().close().await;
    let stored = stored?;
    let proposed = Proposed {
        batch: stored.id.to_string(),
        proposals: stored.proposals.len(),
        database: identity.name,
    };
    stage.book.write(PROPOSED, &proposed).await?;
    Ok(format!(
        "proposed batch {} with {} proposals in {}\n",
        proposed.batch, proposed.proposals, proposed.database
    ))
}

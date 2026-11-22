mod catalog;
#[cfg(test)]
mod fixture;
mod passages;
mod pg;
mod pieces;
mod sqlite;
mod values;
#[cfg(test)]
mod verify;

use std::collections::{BTreeMap, BTreeSet};

use kernel::owner::Owner;
use memory_sqlite::SqliteStore;
use sqlx::PgPool;
use strum::IntoEnumIterator as _;
use tokio_util::sync::CancellationToken;

use self::catalog::Relation;
use self::catalog::Table;
use crate::Error;

type Row = Vec<Option<String>>;
type Counts = BTreeMap<String, u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    ToPostgres,
    ToSqlite,
}

#[derive(Debug)]
struct Batch {
    table: Table,
    rows: Vec<Row>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct TransferReport {
    pub read: Counts,
    pub added: Counts,
    pub committed: bool,
    pub embeddings: &'static str,
    #[serde(flatten)]
    pub omissions: Omissions,
    pub excluded: &'static str,
}

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct Omissions {
    pub claims_on_other_topics: u64,
    pub redirects_owned_by_others: u64,
    pub unattributed_topic_tombstones: u64,
}

pub(super) async fn run(
    pool: &PgPool,
    local: &SqliteStore,
    [owner, local_owner]: [&Owner; 2],
    direction: Direction,
    (dry_run, index_key): (bool, Option<[u8; 32]>),
    shutdown: &CancellationToken,
) -> Result<TransferReport, Error> {
    run_with_limits(
        pool,
        local,
        [owner, local_owner],
        direction,
        (dry_run, index_key, pieces::Limits::DEFAULT),
        shutdown,
    )
    .await
}

async fn run_with_limits(
    pool: &PgPool,
    local: &SqliteStore,
    [owner, local_owner]: [&Owner; 2],
    direction: Direction,
    (dry_run, index_key, limits): (bool, Option<[u8; 32]>, pieces::Limits),
    shutdown: &CancellationToken,
) -> Result<TransferReport, Error> {
    let mut remote = pg::begin(pool, direction).await?;
    let tables = plan(&mut remote, local).await?;
    let owner = owner.as_str();
    let local_owner = local_owner.as_str();
    let (read, added, omissions) = match direction {
        Direction::ToPostgres => {
            let (mut batches, seeds, omissions) =
                sqlite::snapshot(local, tables, local_owner, shutdown.clone()).await?;
            remap_owner(&mut batches, local_owner, owner);
            passages::align(&mut batches, index_key);
            let (read, added) = pg::load(
                &mut remote,
                &mut batches,
                seeds,
                owner,
                (index_key, limits),
                shutdown,
            )
            .await?;
            cancelled(shutdown)?;
            if dry_run {
                remote.rollback().await.map_err(pg::error)?;
            } else {
                remote.commit().await.map_err(pg::error)?;
            }
            (read, added, omissions)
        }
        Direction::ToSqlite => {
            let seeds = pg::seeds(&mut remote).await?;
            let mut batches = pg::snapshot(&mut remote, tables, owner, shutdown).await?;
            let omitted = pg::omissions(&mut remote, owner, &batches, limits).await?;
            remote.rollback().await.map_err(pg::error)?;
            let read = counts(&batches);
            remap_owner(&mut batches, owner, local_owner);
            passages::align(&mut batches, index_key);
            let added = sqlite::load(
                local,
                batches,
                seeds,
                local_owner,
                (dry_run, index_key),
                shutdown.clone(),
            )
            .await?;
            (read, added, omitted)
        }
    };
    Ok(TransferReport {
        read,
        added,
        committed: !dry_run,
        omissions,
        embeddings: "Entity and compatible summary passage vectors travel at destination precision. Existing vectors are kept. The destination rebuilds mismatched passage vectors and other search embeddings and indexes.",
        excluded: "Excluded: other people's rows and memberships, spaces outside the selected membership, global type definitions, unused global tag vocabulary, claims on other people’s topics, wiki redirects owned by someone else or without an exported target, unattributed topic tombstones, archived tasks and delegation runs, Git-backed wiki files and history, wiki recompile revisions, worker pauses, runs and reports, queued jobs, flags, failures, priorities and leases, message chunks, the summary conversion controller, merge and migration bookkeeping, hub state, role principals, and membership authority. Message counts and embedding retry state are rebuilt.",
    })
}

async fn plan(conn: &mut sqlx::PgConnection, local: &SqliteStore) -> Result<Vec<Table>, Error> {
    let mut local = sqlite::columns(local).await?;
    let mut plan = Vec::new();
    for relation in Relation::iter() {
        let names = local.remove(&relation.sqlite()).unwrap_or_default();
        if names.is_empty() {
            return Err(invalid(format!(
                "missing SQLite table {}",
                relation.sqlite()
            )));
        }
        let columns = pg::columns(conn, relation, &names).await?;
        let table = Table {
            relation,
            local: names,
            columns,
        };
        let missing = (matches!(
            relation,
            Relation::TopicTombstones | Relation::WikiRedirects | Relation::TagTaxonomy
        ) && position(&table, "owner_user_id").is_none())
            || (relation == Relation::WikiTopics
                && ["visibility", "scope", "local_slug", "archived_sections"]
                    .into_iter()
                    .any(|name| position(&table, name).is_none()));
        if missing || table.primary_key().is_empty() || table.columns.is_empty() {
            return Err(invalid(format!(
                "missing transferable schema for {}",
                relation.pg()
            )));
        }
        plan.push(table);
    }
    Ok(plan)
}

fn counts(batches: &[Batch]) -> Counts {
    batches
        .iter()
        .map(|batch| (batch.table.relation.pg(), batch.rows.len() as u64))
        .collect()
}

fn cancelled(token: &CancellationToken) -> Result<(), Error> {
    if token.is_cancelled() {
        return Err(invalid("transfer cancelled"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::MergeStore(message.into())
}

fn identifier(name: &str) -> Result<&str, Error> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(invalid(format!("unexpected database identifier {name}")));
    }
    Ok(name)
}

type Seeds = BTreeMap<String, String>;

fn remap_topics(batches: &mut [Batch], source: Seeds, destination: &Seeds) -> Result<(), String> {
    let by_id: BTreeMap<_, _> = source.into_iter().map(|(slug, id)| (id, slug)).collect();
    let mut missing = BTreeSet::new();
    for batch in batches {
        for (index, column) in batch.table.columns.iter().enumerate() {
            if !topic_reference(batch.table.relation, &column.name) {
                continue;
            }
            for row in &mut batch.rows {
                if let Some(value) = row.get_mut(index).and_then(Option::as_mut)
                    && let Some(slug) = by_id.get(value)
                {
                    if let Some(id) = destination.get(slug) {
                        value.clone_from(id);
                    } else {
                        missing.insert(slug.as_str());
                    }
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        let slugs = missing.into_iter().collect::<Vec<_>>().join(", ");
        Err(format!("destination is missing global types: {slugs}"))
    }
}

fn topic_reference(relation: Relation, name: &str) -> bool {
    matches!(
        name,
        "topic_id" | "from_topic" | "to_topic" | "applied_topic"
    ) || (name == "parent_id"
        && matches!(relation, Relation::Topics | Relation::SessionSuggestions))
        || (name == "entity_id" && relation != Relation::Tags)
}

fn position(table: &Table, name: &str) -> Option<usize> {
    table.columns.iter().position(|column| column.name == name)
}

fn cell(row: &Row, position: Option<usize>) -> Option<&str> {
    row.get(position?).and_then(Option::as_deref)
}

#[cfg(test)]
mod tests;

fn remap_owner(batches: &mut [Batch], from: &str, to: &str) {
    if from == to {
        return;
    }
    for batch in batches {
        for (index, column) in batch.table.columns.iter().enumerate() {
            if !batch.table.relation.principal_column(&column.name) {
                continue;
            }
            for row in &mut batch.rows {
                if let Some(value) = row.get_mut(index).and_then(Option::as_mut)
                    && value == from
                {
                    to.clone_into(value);
                }
            }
        }
    }
}

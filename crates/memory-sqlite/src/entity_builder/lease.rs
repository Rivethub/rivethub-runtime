use async_trait::async_trait;
use jiff::Timestamp;
use kernel::wiki::entity_pass::{
    AppliedMark, EntityLease, EntityPage, EntityProse, EntityStoreError, MentionNote,
    PublishOutcome, StoredRewrite, older_than_mark, prose_article,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use uuid::Uuid;

use super::{at, claims, error, locks, pages, query, read, stamp, types, uuid, work, write};
use crate::SqliteStore;

pub(super) struct Lease {
    store: SqliteStore,
    guard: locks::Guard,
}

impl Lease {
    pub(super) fn new(store: SqliteStore, guard: locks::Guard) -> Self {
        Self { store, guard }
    }
}

#[async_trait]
impl EntityLease for Lease {
    async fn page(&mut self) -> Result<EntityPage, EntityStoreError> {
        let key = self.guard.key()?;
        read(&self.store, move |conn| {
            key.check(conn)?;
            let id = key.id;
            let current = claims::current(conn, id)?;
            let view = pages::view(conn, id)?;
            let prose = pages::load_prose(conn, &view)?;
            Ok(EntityPage {
                summary: prose.summary,
                article: prose_article(&prose.article),
                facts: current.facts,
                fixed: current.fixed,
                observed_types: types::observed(conn, id)?,
            })
        })
        .await
    }

    async fn applied_through(&mut self) -> Result<Option<AppliedMark>, EntityStoreError> {
        let key = self.guard.key()?;
        read(&self.store, move |conn| {
            key.check(conn)?;
            mark(conn, key.id)
        })
        .await
    }

    async fn apply(
        &mut self,
        body: &StoredRewrite,
        now: Timestamp,
    ) -> Result<PublishOutcome, EntityStoreError> {
        let key = self.guard.key()?;
        let body = body.clone();
        let Some(directory) = self
            .store
            .wiki_directory()
            .map(std::path::Path::to_path_buf)
        else {
            return write(&self.store, move |tx| {
                key.check(tx)?;
                apply(tx, key.id, &body, now)
            })
            .await;
        };
        let body = body.clone();
        super::filed::commit_filed(
            &self.store,
            &directory,
            key,
            now,
            move |tx| {
                let filed = stage_rewrite(tx, key.id, &body)?;
                Ok(Some((filed.view, filed.prose)))
            },
            move |tx, view, page, sha| pages::store_rendered(tx, view, page, sha, now),
        )
        .await?
        .ok_or_else(|| error(std::io::Error::other("entity rewrite produced no page")))
    }

    async fn record_mention(
        &mut self,
        note: &MentionNote,
        now: Timestamp,
    ) -> Result<Option<String>, EntityStoreError> {
        let key = self.guard.key()?;
        let note = note.clone();
        let Some(directory) = self
            .store
            .wiki_directory()
            .map(std::path::Path::to_path_buf)
        else {
            return write(&self.store, move |tx| {
                key.check(tx)?;
                mention(tx, key.id, &note, now)
            })
            .await;
        };
        let note = note.clone();
        match super::filed::commit_filed(
            &self.store,
            &directory,
            key,
            now,
            move |tx| Ok(stage_mention(tx, key.id, &note)?.map(|filed| (filed.view, filed.prose))),
            move |tx, view, page, sha| pages::store_rendered(tx, view, page, sha, now),
        )
        .await?
        {
            None => Ok(None),
            Some(PublishOutcome::Stored { git_sha, .. }) => Ok(Some(git_sha)),
            Some(PublishOutcome::Stale(_)) => Err(error(std::io::Error::other(
                "entity changed during mention storage",
            ))),
        }
    }

    async fn release(&mut self) -> Result<(), EntityStoreError> {
        self.guard.release().await
    }
}

struct Filed {
    view: kernel::wiki::entity_pass::EntityView,
    prose: EntityProse,
}

fn apply(
    tx: &Transaction<'_>,
    id: Uuid,
    body: &StoredRewrite,
    now: Timestamp,
) -> Result<PublishOutcome, EntityStoreError> {
    let filed = stage_rewrite(tx, id, body)?;
    pages::store(tx, &filed.view, &filed.prose, now)
}

fn stage_rewrite(
    tx: &Transaction<'_>,
    id: Uuid,
    body: &StoredRewrite,
) -> Result<Filed, EntityStoreError> {
    if body.owner.trim().is_empty() {
        return Err(error(std::io::Error::other("entity rewrite has no owner")));
    }
    let inputs = std::iter::once(body.session_id)
        .chain(body.history.iter().map(|line| line.summary_id))
        .chain(body.facts.iter().map(|fact| fact.summary_id));
    work::guard(tx, inputs, &body.owner)?;
    let view = pages::view(tx, id)?;
    let older =
        mark(tx, id)?.is_some_and(|mark| older_than_mark(body.session_at, body.session_id, mark));
    claims::assert_snapshot(tx, id, &body.facts, &body.evidence, &body.owner)?;
    if !older {
        advance(tx, id, body.session_at, body.session_id, &body.owner)?;
    }
    if let Some(line) = &body.history {
        pages::write_history(tx, id, line, &body.owner, true)?;
    }
    let prose = if older {
        current_prose(tx, &view)?
    } else {
        EntityProse {
            summary: body.summary.clone(),
            article: body.article.clone(),
        }
    };
    Ok(Filed {
        view: pages::view(tx, id)?,
        prose,
    })
}

fn mention(
    tx: &Transaction<'_>,
    id: Uuid,
    note: &MentionNote,
    now: Timestamp,
) -> Result<Option<String>, EntityStoreError> {
    let Some(filed) = stage_mention(tx, id, note)? else {
        return Ok(None);
    };
    match pages::store(tx, &filed.view, &filed.prose, now)? {
        PublishOutcome::Stored { git_sha, .. } => Ok(Some(git_sha)),
        PublishOutcome::Stale(_) => Err(error(std::io::Error::other(
            "entity changed during mention storage",
        ))),
    }
}

fn stage_mention(
    tx: &Transaction<'_>,
    id: Uuid,
    note: &MentionNote,
) -> Result<Option<Filed>, EntityStoreError> {
    if note.owner.trim().is_empty() {
        return Err(error(std::io::Error::other("entity mention has no owner")));
    }
    let inputs =
        std::iter::once(note.session_id).chain(note.history.iter().map(|line| line.summary_id));
    work::guard(tx, inputs, &note.owner)?;
    let view = pages::view(tx, id)?;
    advance(tx, id, note.session_at, note.session_id, &note.owner)?;
    let Some(line) = &note.history else {
        return Ok(None);
    };
    pages::write_history(tx, id, line, &note.owner, true)?;
    let prose = current_prose(tx, &view)?;
    Ok(Some(Filed {
        view: pages::view(tx, id)?,
        prose,
    }))
}

fn current_prose(
    conn: &Connection,
    view: &kernel::wiki::entity_pass::EntityView,
) -> Result<EntityProse, EntityStoreError> {
    let mut prose = pages::load_prose(conn, view)?;
    prose.article = prose_article(&prose.article);
    Ok(prose)
}

fn mark(conn: &Connection, id: Uuid) -> Result<Option<AppliedMark>, EntityStoreError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT applied_through, summary_id FROM rh_entity_applied WHERE entity_id = ?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(query)?;
    row.map(|(time, summary)| {
        Ok(AppliedMark {
            at: at(&time)?,
            summary_id: uuid(&summary)?,
        })
    })
    .transpose()
}

fn advance(
    conn: &Connection,
    id: Uuid,
    when: Timestamp,
    summary: Uuid,
    owner: &str,
) -> Result<(), EntityStoreError> {
    conn.execute(
        "INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (entity_id) DO UPDATE SET applied_through = excluded.applied_through,
            summary_id = excluded.summary_id, owner_user_id = excluded.owner_user_id
         WHERE (rh_instant(rh_entity_applied.applied_through), rh_entity_applied.summary_id)
             < (rh_instant(excluded.applied_through), excluded.summary_id)",
        params![id.to_string(), stamp(when), summary.to_string(), owner],
    )
    .map_err(query)?;
    Ok(())
}

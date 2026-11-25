use std::path::Path;
use std::path::PathBuf;

use jiff::Timestamp;
use kernel::topics::PageEdit;
use kernel::topics::TopicId;
use kernel::topics::TopicPage;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::page::WikiSlug;
use rusqlite::Connection;
use rusqlite::params;
use uuid::Uuid;
use wiki_files::wiki_writer::LockedRepository;

use crate::SqliteStore;
use crate::time;
use crate::topic_pages::Claim;
use crate::topic_pages::read;
use crate::topic_pages::save;
use crate::wiki_builder::query;

pub(crate) async fn save_filed(
    store: &SqliteStore,
    root: &Path,
    topic: TopicId,
    page: &TopicPage,
    now: Timestamp,
    claim: Claim,
) -> Result<bool, WikiStoreError> {
    let origin = kernel::wiki::origin::NoteOrigin::current();
    let path = crate::page_files::database_path(store).await?;
    let page = page.clone();
    let locked = crate::page_files::Files::wiki(root).lock().await?;
    crate::page_files::blocking_locked(locked, move |locked| {
        let mut held = crate::page_files::HeldTx::open(&path)?;
        let tx = held.immediate()?;
        share(&tx, origin)?;
        if !claim.allows(&tx, topic, &page.slug)? {
            return Ok(false);
        }
        let existing = read(&tx, &page.slug)?;
        if let Some(existing) = existing.as_ref() {
            tokio::runtime::Handle::current().block_on(crate::wiki_materialize::record_stored(
                locked,
                &tx,
                &existing.slug,
                now,
            ))?;
        }
        let (stored, sha) = tokio::runtime::Handle::current()
            .block_on(crate::wiki_page_files::rewrite(locked, &page, now))?;
        crate::page_files::pause_before_row();
        crate::wiki_page_files::upsert(&tx, &stored, &sha, now)?;
        save(&tx, topic, &page, &time::stamp(now))?;
        tx.commit().map_err(query)?;
        Ok(true)
    })
    .await
}

pub(crate) async fn edit_filed(
    store: &SqliteStore,
    root: &Path,
    slug: &WikiSlug,
    edit: &PageEdit,
    now: Timestamp,
    origin: Option<Uuid>,
) -> Result<bool, WikiStoreError> {
    let path = crate::page_files::database_path(store).await?;
    let slug = slug.clone();
    let edit = edit.clone();
    let locked = crate::page_files::Files::wiki(root).lock().await?;
    crate::page_files::blocking_locked(locked, move |locked| {
        let mut held = crate::page_files::HeldTx::open(&path)?;
        let tx = held.immediate()?;
        share(&tx, origin)?;
        let existing = read(&tx, &slug)?;
        if let Some(existing) = existing.as_ref() {
            tokio::runtime::Handle::current().block_on(crate::wiki_materialize::record_stored(
                locked,
                &tx,
                &existing.slug,
                now,
            ))?;
        }
        let (stored, sha) = tokio::runtime::Handle::current()
            .block_on(crate::wiki_page_files::revise(locked, &slug, &edit, now))?;
        crate::page_files::pause_before_row();
        crate::wiki_page_files::upsert(&tx, &stored, &sha, now)?;
        tx.commit().map_err(query)?;
        Ok(true)
    })
    .await
}

pub(crate) struct FiledEdit<'a> {
    pub(crate) root: TopicId,
    pub(crate) before: &'a TopicPage,
    pub(crate) after: &'a TopicPage,
    pub(crate) now: Timestamp,
    pub(crate) origin: Option<Uuid>,
}

pub(crate) async fn change_locked(
    store: &SqliteStore,
    path: &Path,
    edit: FiledEdit<'_>,
) -> Result<bool, WikiStoreError> {
    let edit = StoredEdit {
        database: crate::page_files::database_path(store).await?,
        before: edit.before.clone(),
        after: edit.after.clone(),
        root: edit.root,
        now: edit.now,
        origin: edit.origin,
    };
    let locked = crate::page_files::Files::wiki(path).lock().await?;
    crate::page_files::blocking_locked(locked, move |locked| change_filed(locked, &edit)).await
}

struct StoredEdit {
    database: PathBuf,
    before: TopicPage,
    after: TopicPage,
    root: TopicId,
    now: Timestamp,
    origin: Option<Uuid>,
}

fn change_filed(locked: &mut LockedRepository, edit: &StoredEdit) -> Result<bool, WikiStoreError> {
    let mut held = crate::page_files::HeldTx::open(&edit.database)?;
    let tx = held.immediate()?;
    share(&tx, edit.origin)?;
    let existing = read(&tx, &edit.before.slug)?;
    let current =
        match tokio::runtime::Handle::current().block_on(locked.read(edit.before.slug.as_ref())) {
            Ok(Some(page)) => Some(crate::wiki_page_files::to_topic(page, &edit.before.slug)?),
            Ok(None) => existing.clone(),
            Err(error) => return Err(WikiStoreError::from(error)),
        };
    if current.as_ref() != Some(&edit.before) {
        return Ok(false);
    }
    if let Some(existing) = existing.as_ref() {
        tokio::runtime::Handle::current().block_on(crate::wiki_materialize::record_stored(
            locked,
            &tx,
            &existing.slug,
            edit.now,
        ))?;
    }
    let (stored, sha) = tokio::runtime::Handle::current().block_on(
        crate::wiki_page_files::rewrite(locked, &edit.after, edit.now),
    )?;
    crate::page_files::pause_before_row();
    crate::wiki_page_files::upsert(&tx, &stored, &sha, edit.now)?;
    save(&tx, edit.root, &edit.after, &time::stamp(edit.now))?;
    tx.commit().map_err(query)?;
    Ok(true)
}

pub(crate) fn delete_topic(conn: &mut Connection, slug: &str) -> Result<(), WikiStoreError> {
    let tx = conn.transaction().map_err(query)?;
    for statement in [
        "DELETE FROM ros_wiki_citations WHERE topic_slug = ?1",
        "DELETE FROM ros_wiki_provenance WHERE topic_slug = ?1",
        "DELETE FROM ros_wiki_redirects WHERE to_slug = ?1",
        "DELETE FROM ros_wiki_topics WHERE slug = ?1",
    ] {
        tx.execute(statement, params![slug]).map_err(query)?;
    }
    tx.commit().map_err(query)?;
    Ok(())
}

fn share(tx: &rusqlite::Transaction<'_>, origin: Option<Uuid>) -> Result<(), WikiStoreError> {
    if let Some(id) = origin {
        crate::summary_replace::shared(tx, id).map_err(crate::wiki_builder::fence_error)?;
    }
    Ok(())
}

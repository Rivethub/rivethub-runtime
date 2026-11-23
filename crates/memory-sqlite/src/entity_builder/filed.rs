use std::path::Path;

use jiff::Timestamp;
use kernel::topics::TopicPage;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::entity_pass::{
    EntityProse, EntityStoreError, EntityView, PublishOutcome, entity_page,
};
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSlug;
use rusqlite::Connection;
use wiki_files::wiki_writer::LockedRepository;

use super::locks::Key;
use super::{error, pages, query, store_error};
use crate::SqliteStore;

pub(super) struct Publication<'a> {
    pub(super) key: Key,
    pub(super) view: &'a EntityView,
    pub(super) prose: &'a EntityProse,
    pub(super) now: Timestamp,
}

pub(super) async fn publish(
    store: &SqliteStore,
    directory: &Path,
    view: &EntityView,
    prose: &EntityProse,
    now: Timestamp,
) -> Result<PublishOutcome, EntityStoreError> {
    let mut guard = super::locks::Guard::acquire(store, view.id).await?;
    let publication = Publication {
        key: guard.key()?,
        view,
        prose,
        now,
    };
    let outcome = publish_held(store, directory, &publication).await;
    guard.release().await?;
    outcome
}

async fn publish_held(
    store: &SqliteStore,
    directory: &Path,
    publication: &Publication<'_>,
) -> Result<PublishOutcome, EntityStoreError> {
    let key = publication.key;
    let now = publication.now;
    let view = publication.view.clone();
    let prose = publication.prose.clone();
    let path = crate::page_files::database_path(store)
        .await
        .map_err(store_error)?;
    let locked = crate::page_files::Files::entity(directory)
        .lock()
        .await
        .map_err(store_error)?;
    locked_unit(locked, move |locked| {
        let mut held = crate::page_files::HeldTx::open(&path).map_err(store_error)?;
        let tx = held.immediate().map_err(store_error)?;
        hold_lease(key, &tx)?;
        match pages::stage_publish(&tx, &view)? {
            pages::Staged::Stale(fresh) => Ok(PublishOutcome::Stale(fresh)),
            pages::Staged::Ready(fresh) => {
                let (page, sha) = file_under(locked, &tx, &fresh, &prose, now)?;
                crate::page_files::pause_before_row();
                let stored = pages::store_rendered(&tx, &fresh, &page, &sha, now)?;
                tx.commit().map_err(query)?;
                Ok(stored)
            }
        }
    })
    .await
}

pub(super) async fn commit_filed(
    store: &SqliteStore,
    directory: &Path,
    key: Key,
    now: Timestamp,
    prepare: impl FnOnce(
        &rusqlite::Transaction<'_>,
    ) -> Result<Option<(EntityView, EntityProse)>, EntityStoreError>
    + Send
    + 'static,
    store_rows: impl FnOnce(
        &rusqlite::Transaction<'_>,
        &EntityView,
        &WikiPage,
        &str,
    ) -> Result<PublishOutcome, EntityStoreError>
    + Send
    + 'static,
) -> Result<Option<PublishOutcome>, EntityStoreError> {
    let path = crate::page_files::database_path(store)
        .await
        .map_err(store_error)?;
    let locked = crate::page_files::Files::entity(directory)
        .lock()
        .await
        .map_err(store_error)?;
    locked_unit(locked, move |locked| {
        let mut held = crate::page_files::HeldTx::open(&path).map_err(store_error)?;
        let tx = held.immediate().map_err(store_error)?;
        hold_lease(key, &tx)?;
        let Some((view, prose)) = prepare(&tx)? else {
            tx.commit().map_err(query)?;
            return Ok(None);
        };
        let (page, sha) = file_under(locked, &tx, &view, &prose, now)?;
        crate::page_files::pause_before_row();
        let stored = store_rows(&tx, &view, &page, &sha)?;
        tx.commit().map_err(query)?;
        Ok(Some(stored))
    })
    .await
}

async fn locked_unit<T, F>(locked: LockedRepository, body: F) -> Result<T, EntityStoreError>
where
    T: Send + 'static,
    F: FnOnce(&mut LockedRepository) -> Result<T, EntityStoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut locked = locked;
        let result = body(&mut locked);
        tokio::runtime::Handle::current().block_on(release(locked, result))
    })
    .await
    .map_err(|join| error(std::io::Error::other(join.to_string())))?
}

fn hold_lease(key: Key, tx: &rusqlite::Transaction<'_>) -> Result<(), EntityStoreError> {
    key.check(tx)?;
    if key.renew(tx)? {
        Ok(())
    } else {
        Err(error(std::io::Error::other(
            "entity lease is no longer held",
        )))
    }
}

fn file_under(
    locked: &mut LockedRepository,
    tx: &rusqlite::Transaction<'_>,
    view: &EntityView,
    prose: &EntityProse,
    now: Timestamp,
) -> Result<(WikiPage, String), EntityStoreError> {
    let stored = load_page(tx, &view.slug)?;
    tokio::runtime::Handle::current().block_on(materialize(locked, tx, stored.as_ref(), now))?;
    tokio::runtime::Handle::current().block_on(fold_entity(locked, view, prose, now))
}

pub(super) async fn fold_entity(
    locked: &mut LockedRepository,
    view: &EntityView,
    prose: &EntityProse,
    now: Timestamp,
) -> Result<(WikiPage, String), EntityStoreError> {
    let mut page = entity_page(view, prose)?;
    let message = format!("entity {}", view.slug);
    let sha = locked
        .fold(&mut page, &[], &message, now)
        .await
        .map_err(WikiStoreError::from)
        .map_err(store_error)?;
    Ok((page, sha))
}

async fn materialize(
    locked: &mut LockedRepository,
    conn: &rusqlite::Connection,
    stored: Option<&TopicPage>,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let Some(stored) = stored else {
        return Ok(());
    };
    crate::wiki_materialize::record_stored(locked, conn, &stored.slug, now)
        .await
        .map_err(store_error)
}

fn load_page(conn: &Connection, slug: &str) -> Result<Option<TopicPage>, EntityStoreError> {
    let slug = WikiSlug::try_from(slug.to_owned())
        .map_err(|_| EntityStoreError::SlugTaken(slug.to_owned()))?;
    crate::topic_pages::read(conn, &slug).map_err(store_error)
}

async fn release<T>(
    locked: LockedRepository,
    result: Result<T, EntityStoreError>,
) -> Result<T, EntityStoreError> {
    let released = locked
        .release()
        .await
        .map_err(WikiStoreError::from)
        .map_err(store_error);
    match (result, released) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(release)) => {
            tracing::warn!(%release, "wiki repository release failed");
            Err(error)
        }
    }
}

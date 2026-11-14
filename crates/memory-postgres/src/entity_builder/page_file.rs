use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::store_error;
use crate::PostgresMemory;
use kernel::wiki::entity_pass::EntityStoreError;

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PageFile {
    pub(super) directory: String,
    pub(super) local: String,
}

pub(super) async fn absorbed_page(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    slug: &str,
) -> Result<Option<PageFile>, sqlx::Error> {
    sqlx::query_as::<_, PageFile>(
        "SELECT coalesce(s.directory, '') AS directory,
                coalesce(t.local_slug, t.slug) AS local
           FROM rh_wiki_topics t
           LEFT JOIN rh_wiki_scopes s ON s.scope = t.scope
          WHERE t.topic_id = $1 OR t.slug = $2 OR t.local_slug = $2
          ORDER BY (t.topic_id = $1) DESC, t.updated_at DESC
          LIMIT 1",
    )
    .bind(id)
    .bind(slug)
    .fetch_optional(&mut **tx)
    .await
}

pub(super) async fn remove_page_file(
    memory: &PostgresMemory,
    page: &PageFile,
) -> Result<(), EntityStoreError> {
    let Some(root) = memory.wiki_directory.clone() else {
        return Ok(());
    };
    let directory = if page.directory.is_empty() {
        root
    } else {
        root.join(&page.directory)
    };
    let (removed, released) = {
        let mut repository = crate::wiki_writer::WikiRepository::new(directory)
            .lock()
            .await
            .map_err(write_error)?;
        let message = format!("entity merge: remove {}", page.local);
        let removed = repository
            .remove(std::slice::from_ref(&page.local), &message)
            .await;
        (removed, repository.release().await)
    };
    removed.map_err(write_error)?;
    released.map_err(write_error)
}

fn write_error(error: kernel::wiki::builder::WikiWriteError) -> EntityStoreError {
    store_error(kernel::wiki::builder::WikiStoreError::Page(error))
}

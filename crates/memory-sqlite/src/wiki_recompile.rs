use async_trait::async_trait;
use jiff::Timestamp;
use kernel::memory::summary_reference::SummaryReferenceError;
use kernel::owner::Owner;
use kernel::wiki::builder::{ExtractWiki, WikiStoreError, WikiSummary};
use kernel::wiki::page::{WikiPage, WikiSlug};
use kernel::wiki::recompile::{
    RecompileSource, Recompiled, SourceSummary, WikiRecompileStore, rebase_recompiled,
};
use rusqlite::{Connection, Transaction};
use wiki_files::wiki_writer::{LockedRepository, WikiRepository};

use crate::SqliteStore;
use crate::page_files::{Files, HeldTx, blocking_locked, database_path};
use crate::wiki_builder::{blocking, fence_error, load_summary, query};

const SUMMARIES: u32 = 400;

#[async_trait]
impl WikiRecompileStore for SqliteStore {
    async fn recompile_source(
        &self,
        slug: &WikiSlug,
        since: Timestamp,
    ) -> Result<Option<RecompileSource>, WikiStoreError> {
        let directory = self
            .wiki_directory()
            .ok_or(WikiStoreError::DirectoryMissing)?;
        let repository = WikiRepository::for_store(directory.to_path_buf());
        let named = slug.clone();
        let unrecorded = blocking(self, move |conn| unrecorded_page(conn, &named)).await?;
        let page = match unrecorded {
            Some(page) => page,
            None => match repository.read_unlocked(slug).await? {
                Some(page) => page,
                None => return Ok(None),
            },
        };
        let history = repository.history_since(slug, since).await?;
        let slug = slug.clone();
        let summaries = blocking(self, move |conn| source_summaries(conn, &slug, since)).await?;
        Ok(Some(RecompileSource {
            page,
            history,
            summaries,
        }))
    }

    async fn store_recompiled(
        &self,
        recompiled: Recompiled,
        now: Timestamp,
    ) -> Result<String, WikiStoreError> {
        let directory = self
            .wiki_directory()
            .ok_or(WikiStoreError::DirectoryMissing)?;
        let path = database_path(self).await?;
        let locked = Files::wiki(directory).lock().await?;
        blocking_locked(locked, move |locked| {
            let mut held = HeldTx::open(&path)?;
            let tx = held.immediate()?;
            let sha = store_locked(locked, &tx, recompiled, now)?;
            tx.commit().map_err(query)?;
            Ok(sha)
        })
        .await
    }
}

fn unrecorded_page(conn: &Connection, slug: &WikiSlug) -> Result<Option<WikiPage>, WikiStoreError> {
    let stored = crate::wiki_read::database_page(conn, slug.as_ref())
        .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
    Ok(stored
        .filter(|stored| stored.git_sha.is_none())
        .map(|stored| stored.page))
}

fn source_summaries(
    conn: &Connection,
    slug: &WikiSlug,
    since: Timestamp,
) -> Result<Vec<SourceSummary>, WikiStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT s.id, p.topic_id FROM ros_wiki_citations w
             JOIN ros_summaries s ON s.id = w.summary_id
             LEFT JOIN ros_conversations c ON c.id = s.conversation_id
             LEFT JOIN rh_session_placements p ON p.session_id = c.session_key
             WHERE w.topic_slug = ?1
             ORDER BY rh_instant(s.created_at) DESC, s.id LIMIT ?2",
        )
        .map_err(query)?;
    let rows = statement
        .query_map((slug.as_ref(), SUMMARIES), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(query)?;
    let mut summaries = Vec::new();
    for row in rows {
        let (id, topic) = row.map_err(query)?;
        let job = ExtractWiki {
            summary_id: uuid::Uuid::parse_str(&id)
                .map_err(|error| WikiStoreError::Query(Box::new(error)))?,
            conversation_id: None,
        };
        let input = match load_summary(conn, &job) {
            Ok(input) => input,
            Err(error) if error.obsolete_input() => continue,
            Err(error) => return Err(error),
        };
        summaries.push(SourceSummary {
            topic,
            at: input.created_at.unwrap_or(since),
            content: input.content.clone(),
            input: Some(input),
        });
    }
    Ok(summaries)
}

fn store_locked(
    repository: &mut LockedRepository,
    tx: &Transaction<'_>,
    recompiled: Recompiled,
    now: Timestamp,
) -> Result<String, WikiStoreError> {
    let Recompiled {
        inputs,
        base,
        mut page,
        mut archived,
        message,
    } = recompiled;
    for input in &inputs {
        validate_input(tx, input)?;
    }
    let runtime = tokio::runtime::Handle::current();
    runtime.block_on(crate::wiki_materialize::record_stored(
        repository,
        tx,
        &page.meta.slug,
        now,
    ))?;
    let slug = page.meta.slug.as_ref();
    let current = runtime.block_on(repository.read(slug))?.ok_or_else(|| {
        WikiStoreError::InvalidPatch(format!(
            "wiki page {slug} was removed while it was recompiled"
        ))
    })?;
    if current != base {
        page = rebase_recompiled(&base, page, &current, &mut archived);
    }
    let sha = runtime.block_on(repository.recompile(&mut page, archived, &message, now))?;
    crate::wiki_page_files::upsert(tx, &page, &sha, now)?;
    Ok(sha)
}

fn validate_input(tx: &Transaction<'_>, input: &WikiSummary) -> Result<(), WikiStoreError> {
    let found = crate::summary_replace::shared(tx, input.id).map_err(fence_error)?;
    if found.conversation_id != input.conversation_id.map(|id| id.to_string())
        || found.owner.as_deref() != input.owner.as_ref().map(Owner::as_str)
        || found.content != input.content
        || found.input_key.as_deref()
            != input
                .delivery
                .as_ref()
                .map(|delivery| delivery.input_key.as_slice())
    {
        return Err(SummaryReferenceError::Stale(input.id).into());
    }
    Ok(())
}

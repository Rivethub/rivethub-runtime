use jiff::Timestamp;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::memory::MemoryExport;
use kernel::memory::MemoryStoreError;
use memory_postgres::PostgresMemory;
use memory_postgres::dump::CopySource;
use memory_sqlite::SqliteStore;
use memory_sqlite::import::ImportTable;
use strum::IntoEnumIterator;
use tokio_util::sync::CancellationToken;

use super::io;
use super::target;
use crate::Error;

#[expect(missing_debug_implementations)]
pub enum MemoryExporter {
    Postgres {
        memory: Arc<PostgresMemory>,
        url: String,
    },
    Sqlite(Arc<SqliteStore>),
}

impl MemoryExporter {
    #[must_use]
    pub fn postgres(memory: Arc<PostgresMemory>, url: String) -> Self {
        Self::Postgres { memory, url }
    }

    #[must_use]
    pub fn sqlite(store: Arc<SqliteStore>) -> Self {
        Self::Sqlite(store)
    }

    pub(super) async fn gzipped(
        &self,
        since: Option<Timestamp>,
        shutdown: &CancellationToken,
    ) -> Result<Vec<u8>, Error> {
        let plain = match self {
            Self::Postgres { memory, url } => {
                let origin = target::origin_for_url(url)?;
                memory
                    .export_memory_dump(&copy_sources(), since, &origin, shutdown)
                    .await?
            }
            Self::Sqlite(store) => {
                store
                    .export_pg_dump(since, target::sqlite_origin()?, shutdown.clone())
                    .await?
            }
        };
        Ok(tokio::task::spawn_blocking(move || io::gzip(&plain)).await??)
    }
}

#[async_trait]
impl MemoryExport for MemoryExporter {
    async fn export(
        &self,
        since: Option<Timestamp>,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, MemoryStoreError> {
        self.gzipped(since, &cancellation)
            .await
            .map_err(|error| MemoryStoreError::Query {
                message: error.to_string(),
                source: Box::new(error),
            })
    }
}

fn copy_sources() -> Vec<CopySource<'static>> {
    ImportTable::iter().filter_map(copy_source).collect()
}

fn copy_source(table: ImportTable) -> Option<CopySource<'static>> {
    let columns = table.dump_columns()?;
    let (relation, compatibility) = match table {
        ImportTable::Conversations => ("rh_conversations", "ros_conversations"),
        ImportTable::Messages => ("rh_messages", "ros_messages"),
        ImportTable::Summaries => ("rh_summaries", "ros_summaries"),
        ImportTable::SummarySources => ("rh_summary_sources", "ros_summary_sources"),
        ImportTable::Tags => ("rh_tags", "ros_tags"),
        ImportTable::TagTaxonomy => ("rh_tag_taxonomy", "ros_tag_taxonomy"),
        ImportTable::WikiTopics => ("rh_wiki_topics", "ros_wiki_topics"),
        ImportTable::WikiProvenance => ("rh_wiki_provenance", "ros_wiki_provenance"),
        ImportTable::WikiExtractions => ("rh_wiki_extractions", "ros_wiki_extractions"),
        ImportTable::WikiCitations => ("rh_wiki_citations", "ros_wiki_citations"),
        ImportTable::WikiRedirects => ("rh_wiki_redirects", "ros_wiki_redirects"),
        ImportTable::Jobs | ImportTable::Meta | ImportTable::EmbedQueue => return None,
    };
    Some(CopySource {
        relation,
        compatibility,
        columns,
    })
}

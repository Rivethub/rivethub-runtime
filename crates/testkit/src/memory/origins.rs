use std::collections::BTreeMap;

use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::MemoryEntry;
use kernel::memory::Provenance;
use tokio::sync::Mutex;
use wire::JsonObject;

#[derive(Debug, Default)]
pub(super) struct Origins(Mutex<BTreeMap<String, JsonObject>>);

impl Origins {
    pub(super) async fn remember(&self, batch: &CaptureBatchRequest) {
        if !kernel::memory::is_imported(&batch.session_key) {
            return;
        }
        self.0.lock().await.insert(
            batch.session_key.clone(),
            batch.settings.clone().unwrap_or_default(),
        );
    }

    pub(super) async fn attach(&self, mut entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
        let stored = self.0.lock().await;
        for entry in &mut entries {
            if let Some(settings) = stored.get(&entry.session_id) {
                entry.provenance = Some(Provenance::from_settings(settings));
            }
        }
        entries
    }

    pub(super) async fn lookup(&self, session_keys: Vec<String>) -> BTreeMap<String, Provenance> {
        let stored = self.0.lock().await;
        session_keys
            .into_iter()
            .filter_map(|key| {
                stored
                    .get(&key)
                    .map(|settings| (key, Provenance::from_settings(settings)))
            })
            .collect()
    }
}

pub(super) fn wiki_authors(
    pages: &BTreeMap<String, kernel::wiki::WikiPageResponse>,
    slugs: Vec<String>,
) -> BTreeMap<String, Vec<Provenance>> {
    slugs
        .into_iter()
        .filter_map(|slug| {
            let imported: Vec<_> = pages
                .get(&slug)?
                .citations
                .iter()
                .filter_map(|citation| citation.imported.clone())
                .collect();
            (!imported.is_empty()).then_some((slug, imported))
        })
        .collect()
}

pub(super) fn browsed(filter: &BrowseFilter, entry: &MemoryEntry) -> bool {
    filter.agent.as_deref().is_none_or(|a| entry.agent == a)
        && filter.role.is_none_or(|r| entry.role == r)
        && filter
            .tool_name
            .as_deref()
            .is_none_or(|t| entry.tool_name.as_deref() == Some(t))
}

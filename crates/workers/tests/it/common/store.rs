use std::collections::HashMap;

use async_trait::async_trait;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use tokio::sync::Mutex;
use tokio::sync::watch;
use workers::EmbedTable;
use workers::EmbedTarget;
use workers::LoadedTarget;
use workers::StoredEmbedding;
use workers::Unembeddable;

#[derive(Debug, Clone)]
struct TargetRow {
    loaded: LoadedTarget,
    failures: u32,
    written: Option<StoredEmbedding>,
    unembeddable: Option<Unembeddable>,
    permanent: bool,
    embed_error: Option<String>,
    embed_status: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Store {
    rows: Mutex<HashMap<(EmbedTable, String), TargetRow>>,
    loads: watch::Sender<usize>,
    hold: bool,
    released: watch::Sender<bool>,
}

impl Store {
    pub(crate) fn new() -> Self {
        Self::build(false)
    }

    pub(crate) fn holding() -> Self {
        Self::build(true)
    }

    fn build(hold: bool) -> Self {
        let (loads, _) = watch::channel(0);
        let (released, _) = watch::channel(false);
        Self {
            rows: Mutex::new(HashMap::new()),
            loads,
            hold,
            released,
        }
    }

    pub(crate) async fn insert(
        &self,
        table: EmbedTable,
        id: &str,
        content: &str,
        tool_result: Option<&str>,
    ) -> Result<(), String> {
        let mut rows = self.rows.lock().await;
        let key = (table, id.to_owned());
        if rows.contains_key(&key) {
            return Err(format!("duplicate embed target {id}"));
        }
        rows.insert(
            key,
            TargetRow {
                loaded: LoadedTarget {
                    content: content.to_owned(),
                    tool_result: tool_result.map(str::to_owned),
                },
                failures: 0,
                written: None,
                unembeddable: None,
                permanent: false,
                embed_error: None,
                embed_status: None,
            },
        );
        drop(rows);
        Ok(())
    }

    pub(crate) fn loads(&self) -> watch::Receiver<usize> {
        self.loads.subscribe()
    }

    pub(crate) fn load_count(&self) -> usize {
        *self.loads.subscribe().borrow()
    }

    pub(crate) fn release(&self) {
        let _previous = self.released.send_replace(true);
    }

    pub(crate) async fn written(&self, table: EmbedTable, id: &str) -> Option<StoredEmbedding> {
        self.rows
            .lock()
            .await
            .get(&(table, id.to_owned()))
            .and_then(|row| row.written.clone())
    }

    pub(crate) async fn failures(&self, table: EmbedTable, id: &str) -> u32 {
        self.row(table, id).await.failures
    }

    pub(crate) async fn permanent(&self, table: EmbedTable, id: &str) -> bool {
        self.row(table, id).await.permanent
    }

    pub(crate) async fn embed_error(&self, table: EmbedTable, id: &str) -> Option<String> {
        self.row(table, id).await.embed_error
    }

    pub(crate) async fn embed_status(&self, table: EmbedTable, id: &str) -> Option<String> {
        self.row(table, id).await.embed_status
    }

    pub(crate) async fn unembeddable(&self, table: EmbedTable, id: &str) -> Option<Unembeddable> {
        self.rows
            .lock()
            .await
            .get(&(table, id.to_owned()))
            .and_then(|row| row.unembeddable)
    }

    pub(crate) async fn writes(&self) -> usize {
        self.rows
            .lock()
            .await
            .values()
            .filter(|row| row.written.is_some())
            .count()
    }

    async fn row(&self, table: EmbedTable, id: &str) -> TargetRow {
        self.rows
            .lock()
            .await
            .get(&(table, id.to_owned()))
            .cloned()
            .expect("the embed target exists")
    }

    async fn enter(&self) -> Result<(), EmbeddingStoreError> {
        if !self.hold {
            return Ok(());
        }
        self.loads.send_modify(|count| {
            *count = count.saturating_add(1);
        });
        let mut released = self.released.subscribe();
        while !*released.borrow() {
            released
                .changed()
                .await
                .map_err(|_| EmbeddingStoreError::Closed)?;
        }
        Ok(())
    }
}

fn missing_target(id: &str) -> EmbeddingStoreError {
    EmbeddingStoreError::Missing { id: id.to_owned() }
}

fn target_key(target: &EmbedTarget) -> (EmbedTable, String) {
    (target.table, target.id.clone())
}

#[async_trait]
impl EmbeddingStore for Store {
    async fn load(
        &self,
        target: &EmbedTarget,
    ) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
        self.enter().await?;
        let loaded = self
            .rows
            .lock()
            .await
            .get(&target_key(target))
            .map(|row| row.loaded.clone());
        Ok(loaded)
    }

    async fn write(&self, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
        let mut rows = self.rows.lock().await;
        let Some(row) = rows.get_mut(&target_key(&stored.target)) else {
            return Err(missing_target(&stored.target.id));
        };
        row.written = Some(stored);
        row.failures = 0;
        row.permanent = false;
        row.embed_error = None;
        row.embed_status = None;
        drop(rows);
        Ok(())
    }

    async fn mark_unembeddable(
        &self,
        target: &EmbedTarget,
        reason: Unembeddable,
    ) -> Result<(), EmbeddingStoreError> {
        let mut rows = self.rows.lock().await;
        let Some(row) = rows.get_mut(&target_key(target)) else {
            return Err(missing_target(&target.id));
        };
        row.unembeddable = Some(reason);
        row.embed_status = Some("unembeddable".to_owned());
        row.embed_error = Some(reason.error_text());
        drop(rows);
        Ok(())
    }

    async fn record_null(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<u32, EmbeddingStoreError> {
        let mut rows = self.rows.lock().await;
        let Some(row) = rows.get_mut(&target_key(target)) else {
            return Err(missing_target(&target.id));
        };
        row.failures = row.failures.saturating_add(1);
        row.embed_error = Some(error.to_owned());
        let failures = row.failures;
        drop(rows);
        Ok(failures)
    }

    async fn mark_permanent(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<(), EmbeddingStoreError> {
        let mut rows = self.rows.lock().await;
        let Some(row) = rows.get_mut(&target_key(target)) else {
            return Err(missing_target(&target.id));
        };
        row.permanent = true;
        row.embed_status = Some("failed".to_owned());
        row.embed_error = Some(error.to_owned());
        drop(rows);
        Ok(())
    }
}

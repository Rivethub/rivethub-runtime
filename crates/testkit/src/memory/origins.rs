use std::collections::BTreeMap;

use kernel::memory::CaptureBatchRequest;
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

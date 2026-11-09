use std::collections::BTreeMap;

use kernel::memory::CaptureBatchRequest;
use tokio::sync::Mutex;
use wire::JsonObject;

#[derive(Debug, Default)]
pub(super) struct Origins(Mutex<BTreeMap<String, JsonObject>>);

impl Origins {
    pub(super) async fn remember(&self, batch: &CaptureBatchRequest) {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B6 (note 134): lookup_origins returns only origins that are JSON objects"
            )
        )]
        let origin = batch
            .settings
            .as_ref()
            .and_then(|settings| settings.get("origin"))
            .and_then(|origin| serde_json::from_value::<JsonObject>(origin.clone()).ok());
        if let Some(origin) = origin {
            self.0
                .lock()
                .await
                .insert(batch.session_key.clone(), origin);
        }
    }

    pub(super) async fn lookup(&self, session_keys: Vec<String>) -> BTreeMap<String, JsonObject> {
        let origins = self.0.lock().await;
        session_keys
            .into_iter()
            .filter_map(|key| origins.get(&key).map(|origin| (key, origin.clone())))
            .collect()
    }
}

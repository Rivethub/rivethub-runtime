use std::collections::BTreeMap;

use async_trait::async_trait;
use kernel::memory::MemoryStoreError;
use kernel::model_calls::{ModelCall, ModelCallStore, RecordedCalls};
use kernel::owner::Owner;
use parking_lot::Mutex;
use wire::session_id::SessionId;

#[derive(Debug, Default)]
pub struct FakeModelCallStore {
    rows: Mutex<BTreeMap<String, (String, ModelCall)>>,
}

impl FakeModelCallStore {
    #[must_use]
    pub fn stored(&self) -> Vec<(String, ModelCall)> {
        self.rows.lock().values().cloned().collect()
    }
}

#[async_trait]
impl ModelCallStore for FakeModelCallStore {
    async fn record(
        &self,
        owner: &Owner,
        calls: &[ModelCall],
    ) -> Result<RecordedCalls, MemoryStoreError> {
        let mut rows = self.rows.lock();
        let mut recorded = RecordedCalls::default();
        for call in calls {
            if rows.contains_key(&call.call_id) {
                recorded.duplicate += 1;
            } else {
                rows.insert(
                    call.call_id.clone(),
                    (owner.as_str().to_owned(), call.clone()),
                );
                recorded.inserted += 1;
            }
        }
        drop(rows);
        Ok(recorded)
    }

    async fn session_calls(&self, session: &SessionId) -> Result<Vec<ModelCall>, MemoryStoreError> {
        let mut calls: Vec<ModelCall> = self
            .rows
            .lock()
            .values()
            .filter(|(_, call)| call.session.as_ref() == Some(session))
            .map(|(_, call)| call.clone())
            .collect();
        calls.sort_by(|left, right| {
            (left.started_at, &left.call_id).cmp(&(right.started_at, &right.call_id))
        });
        Ok(calls)
    }
}

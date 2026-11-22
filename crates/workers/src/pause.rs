use std::sync::Arc;

use kernel::queue::WorkerRole;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses;

use crate::error::WorkerError;

#[derive(Default)]
pub(crate) struct Pauses {
    store: Option<Arc<dyn WorkerPauses>>,
    paused: parking_lot::Mutex<Vec<WorkerRole>>,
}

impl Pauses {
    pub(crate) fn reading(store: Arc<dyn WorkerPauses>) -> Self {
        Self {
            store: Some(store),
            paused: parking_lot::Mutex::default(),
        }
    }

    pub(crate) async fn claimable(
        &self,
        roles: &[WorkerRole],
    ) -> Result<Vec<WorkerRole>, WorkerError> {
        let Some(store) = &self.store else {
            return Ok(roles.to_vec());
        };
        if !roles.iter().any(|role| PausableRole::of(*role).is_some()) {
            return Ok(roles.to_vec());
        }
        let paused: Vec<WorkerRole> = store
            .paused()
            .await
            .map_err(WorkerError::queue)?
            .into_iter()
            .map(|pause| pause.role.worker_role())
            .filter(|role| roles.contains(role))
            .collect();
        self.announce(&paused);
        Ok(roles
            .iter()
            .copied()
            .filter(|role| !paused.contains(role))
            .collect())
    }

    fn announce(&self, paused: &[WorkerRole]) {
        let mut seen = self.paused.lock();
        for role in paused.iter().filter(|role| !seen.contains(role)) {
            tracing::info!("{role} is paused; it claims no jobs until it is resumed");
        }
        for role in seen.iter().filter(|role| !paused.contains(role)) {
            tracing::info!("{role} resumed claiming jobs");
        }
        *seen = paused.to_vec();
    }
}

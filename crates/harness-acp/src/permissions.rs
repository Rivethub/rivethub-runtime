use std::collections::BTreeMap;
use uuid::Uuid;

use agent_client_protocol::v1::schema as acp;
use kernel::harness::ApprovalDecision;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::error::AcpError;

#[derive(Debug)]
pub(crate) struct PendingPermission {
    pub(crate) options: Vec<acp::PermissionOption>,
    pub(crate) respond: oneshot::Sender<acp::RequestPermissionOutcome>,
}

#[derive(Debug, Default)]
pub(crate) struct PermissionRegistry {
    pending: Mutex<BTreeMap<String, PendingPermission>>,
}

impl PermissionRegistry {
    pub(crate) fn register(
        &self,
        options: Vec<acp::PermissionOption>,
    ) -> (String, oneshot::Receiver<acp::RequestPermissionOutcome>) {
        let request_id = Uuid::new_v4().to_string();
        let (respond, answer) = oneshot::channel();
        self.pending
            .lock()
            .insert(request_id.clone(), PendingPermission { options, respond });
        (request_id, answer)
    }

    pub(crate) fn withdraw(&self, request_id: &str) {
        self.pending.lock().remove(request_id);
    }

    pub(crate) fn answer(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), AcpError> {
        let wanted = match decision {
            ApprovalDecision::Allow => acp::PermissionOptionKind::AllowOnce,
            ApprovalDecision::AllowSession => acp::PermissionOptionKind::AllowAlways,
            ApprovalDecision::Deny => acp::PermissionOptionKind::RejectOnce,
        };
        let selected = {
            let pending = self.pending.lock();
            pending.get(request_id).and_then(|entry| {
                entry
                    .options
                    .iter()
                    .find(|option| option.kind == wanted)
                    .map(|option| {
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(option.option_id.clone()),
                        )
                    })
            })
        };
        let Some(outcome) = selected else {
            let known = self.pending.lock().contains_key(request_id);
            if known {
                return Err(AcpError::UnanswerablePermission { decision });
            }
            return Err(AcpError::UnknownPermission {
                request_id: request_id.to_owned(),
            });
        };
        let answered = {
            let mut pending = self.pending.lock();
            let entry = pending.remove(request_id);
            drop(pending);
            entry
        };
        let entry = answered.ok_or_else(|| AcpError::UnknownPermission {
            request_id: request_id.to_owned(),
        })?;
        if entry.respond.send(outcome).is_err() {
            return Err(AcpError::Closed {
                method: "session/request_permission",
            });
        }
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        let pending = std::mem::take(&mut *self.pending.lock());
        for (_, entry) in pending {
            if entry
                .respond
                .send(acp::RequestPermissionOutcome::Cancelled)
                .is_err()
            {
                tracing::debug!("permission answer arrived after the turn was cancelled");
            }
        }
    }
}

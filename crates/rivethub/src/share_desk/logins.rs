use std::collections::BTreeMap;
use std::sync::Arc;

use capture::seal::ShareServer;
use chrono::DateTime;
use chrono::Utc;
use cli::memory::desk::PendingLogin;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::LoginId;
use kernel::memory::share::desk::ServerOrigin;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;

pub(super) struct Pending {
    pub(super) expires_at: DateTime<Utc>,
    pub(super) login: Arc<Mutex<PendingLogin>>,
}

pub(super) fn host(origin: &ServerOrigin) -> Result<ShareServer, DeskError> {
    origin
        .as_str()
        .parse()
        .map_err(|error: capture::seal::CodeError| DeskError::BadServer(error.to_string()))
}

#[derive(Default)]
pub(super) struct OriginGates(parking_lot::Mutex<BTreeMap<ShareServer, Arc<Mutex<()>>>>);

impl OriginGates {
    pub(super) async fn hold(&self, host: &ShareServer) -> OwnedMutexGuard<()> {
        let gate = Arc::clone(self.0.lock().entry(host.clone()).or_default());
        gate.lock_owned().await
    }
}

pub(super) async fn cancel(
    logins: &parking_lot::Mutex<BTreeMap<LoginId, Pending>>,
    host: &ShareServer,
) {
    let held: Vec<_> = logins
        .lock()
        .iter()
        .map(|(id, pending)| (*id, Arc::clone(&pending.login)))
        .collect();
    for (id, login) in held {
        let mut login = login.lock().await;
        if login.server() == host {
            login.cancel().await;
            drop(login);
            logins.lock().remove(&id);
        }
    }
}

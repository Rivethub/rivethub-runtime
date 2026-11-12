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

pub(super) struct Pending {
    pub(super) expires_at: DateTime<Utc>,
    pub(super) login: Arc<Mutex<PendingLogin>>,
}

pub(super) async fn cancel(
    logins: &parking_lot::Mutex<BTreeMap<LoginId, Pending>>,
    origin: &ServerOrigin,
) -> Result<(), DeskError> {
    let host: ShareServer = origin
        .as_str()
        .parse()
        .map_err(|error: capture::seal::CodeError| DeskError::BadServer(error.to_string()))?;
    let held: Vec<_> = logins
        .lock()
        .iter()
        .map(|(id, pending)| (*id, Arc::clone(&pending.login)))
        .collect();
    for (id, login) in held {
        let mut login = login.lock().await;
        if *login.server() == host {
            login.cancel().await;
            drop(login);
            logins.lock().remove(&id);
        }
    }
    Ok(())
}

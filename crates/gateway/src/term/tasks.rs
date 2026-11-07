use wire::session_id::SessionId;

use super::TerminalService;

impl TerminalService {
    pub(crate) fn remember_requested(&self, requested: &SessionId, canonical: &SessionId) {
        if requested != canonical {
            self.requested
                .write()
                .insert(requested.as_str().to_owned(), canonical.clone());
        }
    }

    pub(crate) fn holds(&self, session: &str) -> bool {
        crate::sessions::session_key(session)
            .is_ok_and(|key| self.live_chat(&key).is_some() || self.pty_running(&key))
    }
}

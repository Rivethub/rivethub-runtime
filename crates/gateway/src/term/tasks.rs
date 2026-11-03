use kernel::harness::{HarnessAdapterError, SessionOption};
use wire::harness::ApprovalDecision;
use wire::session_id::SessionId;

use super::TerminalService;
use crate::error::ApiError;
use crate::sessions::control::ChatHandle;
use crate::tenancy::UserId;

pub(crate) const BYPASS_PERMISSIONS: &str = "bypassPermissions";
const DEFAULT_PERMISSIONS: &str = "default";

impl TerminalService {
    pub(crate) fn remember_requested(&self, requested: &SessionId, canonical: &SessionId) {
        if requested != canonical {
            self.requested
                .write()
                .insert(requested.as_str().to_owned(), canonical.clone());
        }
    }

    pub(crate) fn canonical(&self, session: &str) -> String {
        self.requested.read().get(session).map_or_else(
            || session.to_owned(),
            |canonical| canonical.as_str().to_owned(),
        )
    }

    pub(crate) fn holds(&self, session: &str) -> bool {
        crate::sessions::session_key(session)
            .is_ok_and(|key| self.live_chat(&key).is_some() || self.pty_running(&key))
    }

    pub(crate) fn live_chat_of(&self, session: &str) -> Option<ChatHandle> {
        match crate::sessions::session_key(session) {
            Ok(key) => self.live_chat(&key),
            Err(error) => {
                tracing::debug!(%error, "not a session key this node can hold");
                None
            }
        }
    }

    pub(crate) async fn permission_mode(&self, session: &str) -> Option<String> {
        let canonical = match session.parse::<SessionId>() {
            Ok(canonical) => canonical,
            Err(error) => {
                tracing::debug!(%error, "a parent session without a canonical session id");
                return None;
            }
        };
        if self.pty_running(canonical.native()) {
            return Some(self.terminal_mode(&canonical).await);
        }
        self.live_chat_of(session)?;
        self.harness.as_ref()?.mode(&canonical)
    }

    async fn terminal_mode(&self, session: &SessionId) -> String {
        let Some(lister) = &self.session_lister else {
            return DEFAULT_PERMISSIONS.to_owned();
        };
        match lister
            .permission_mode(session.harness(), session.native())
            .await
        {
            Ok(Some(mode)) => mode,
            Ok(None) => DEFAULT_PERMISSIONS.to_owned(),
            Err(error) => {
                tracing::debug!(%error, "the terminal's permission mode could not be read");
                DEFAULT_PERMISSIONS.to_owned()
            }
        }
    }

    pub(crate) async fn choose_mode(
        &self,
        session: &SessionId,
        mode: String,
    ) -> Result<(), ApiError> {
        let harness = self
            .harness
            .clone()
            .ok_or_else(|| ApiError::Unavailable("chat harness unavailable".into()))?;
        match harness.set_option(session, SessionOption::Mode(mode)).await {
            Ok(())
            | Err(
                HarnessAdapterError::OptionUnavailable { .. }
                | HarnessAdapterError::OptionRejected { .. },
            ) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) async fn allow(
        &self,
        user: &UserId,
        session: &SessionId,
        request: &str,
    ) -> Result<(), ApiError> {
        self.answer_approval(user, session, request, ApprovalDecision::Allow)
            .await
    }
}

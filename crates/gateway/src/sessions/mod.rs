use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::events::{ProtocolVersion, SessionBroadcast};
use wire::gateway::topics::TopicChangedNotice;

const NOTICE_BUFFER: usize = 256;

#[derive(Debug)]
pub struct SessionRegistry {
    notices: broadcast::Sender<SessionBroadcast>,
    pub(crate) tasks: TaskTracker,
    pub(crate) shutdown: CancellationToken,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "a broadcast sender has no Default, so the notice channel is opened here"
    )
)]
impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            notices: broadcast::channel(NOTICE_BUFFER).0,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }
}

impl SessionRegistry {
    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }

    pub(crate) fn topic_changed(&self, topic: &impl ToString) {
        self.announce(&SessionBroadcast::TopicChanged(TopicChangedNotice {
            v: ProtocolVersion::V1,
            topic: topic.to_string(),
        }));
    }

    pub(crate) fn announce(&self, notice: &SessionBroadcast) {
        if self.notices.send(notice.clone()).is_err() {
            tracing::trace!("notice dropped; no listening client");
        }
    }

    pub fn notices(&self) -> broadcast::Receiver<SessionBroadcast> {
        self.notices.subscribe()
    }

    pub async fn close(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

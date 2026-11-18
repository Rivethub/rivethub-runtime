use jiff::Timestamp;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::LISTING_ORDER;
use super::StoreRoots;
use crate::readers::StoreReader;

#[derive(Debug, Clone)]
pub struct StoreTranscriptReader {
    roots: Arc<StoreRoots>,
}

impl StoreTranscriptReader {
    #[must_use]
    pub fn new(roots: StoreRoots) -> Self {
        Self {
            roots: Arc::new(roots),
        }
    }

    fn reader(&self, harness: HarnessId) -> Result<StoreReader, TranscriptReaderError> {
        self.roots
            .single(harness)
            .and_then(|root| StoreReader::open(harness, root.to_path_buf()))
            .ok_or_else(|| unavailable(format!("{harness} has no transcript store")))
    }

    fn session_reader(&self, session: &SessionId) -> Result<StoreReader, TranscriptReaderError> {
        self.reader(session.harness())
    }

    fn store_reader(&self, store: &Path) -> Result<StoreReader, TranscriptReaderError> {
        let harness = LISTING_ORDER
            .into_iter()
            .find(|harness| self.roots.single(*harness) == Some(store))
            .ok_or_else(|| {
                unavailable(format!(
                    "{} is not a configured transcript store",
                    store.display()
                ))
            })?;
        self.reader(harness)
    }
}

fn unavailable(message: String) -> TranscriptReaderError {
    TranscriptReaderError::Unavailable(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        message,
    )))
}

#[async_trait]
impl TranscriptReader for StoreTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        self.store_reader(store)?.sessions(store).await
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        self.session_reader(session)?.read(session).await
    }

    async fn tail(
        &self,
        session: &SessionId,
        since: Timestamp,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        self.session_reader(session)?.tail(session, since).await
    }
}

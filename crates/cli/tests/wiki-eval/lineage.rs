use async_trait::async_trait;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionRenameError;
use kernel::transcript::StoreSession;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use wire::harness::HarnessId;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct EmptyLineage;

#[async_trait]
impl SessionLister for EmptyLineage {
    async fn list(&self, _harnesses: &[HarnessId], _limit: ListLimit) -> Vec<StoreSession> {
        Vec::new()
    }

    async fn describe(
        &self,
        _harness: HarnessId,
        _native: &str,
        _parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        Ok(None)
    }

    async fn store_cwd(
        &self,
        _harness: HarnessId,
        _native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(None)
    }

    async fn transcript(
        &self,
        _harness: HarnessId,
        _native: &str,
        _window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        Ok(TranscriptRead::NoSource)
    }

    async fn rename(
        &self,
        _harness: HarnessId,
        _native: &str,
        _title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        Err(SessionRenameError::Missing)
    }

    async fn tool_output(
        &self,
        _harness: HarnessId,
        _native: &str,
        _tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        Ok(ToolOutputRead::NoSource)
    }
}

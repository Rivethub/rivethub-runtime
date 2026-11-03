use async_trait::async_trait;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionRenameError;
use kernel::transcript::StoreSession;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use kernel::wiki::builder::ExtractWiki;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::topics::Outcome;
use crate::topics::new_topic;
use crate::topics::same;
use crate::wiki_routes::NoParents;
use crate::wiki_routes::PARENT_KEY;
use crate::wiki_routes::RouteStores;
use crate::wiki_routes::SESSION_KEY;
use crate::wiki_routes::patch;

pub const MIDDLE_KEY: &str = "claude-code:00000000-0000-4000-8000-000000000040";
pub const SUBAGENT_KEY: &str =
    "claude-code:00000000-0000-4000-8000-000000000041/subagents/agent-a1";

#[derive(Debug)]
pub struct Parents(pub &'static [(&'static str, &'static str)]);

pub const CHILD: Parents = Parents(&[(SESSION_KEY, PARENT_KEY)]);
pub const GRANDCHILD: Parents = Parents(&[(SESSION_KEY, MIDDLE_KEY), (MIDDLE_KEY, PARENT_KEY)]);

#[async_trait]
impl SessionLister for Parents {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
        NoParents.list(harnesses, limit).await
    }

    async fn describe(
        &self,
        harness: HarnessId,
        native: &str,
        _parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        let key = SessionId::new(harness, native);
        let mut row = StoreSession::new(harness, native, 0);
        row.parent_session_id = self
            .0
            .iter()
            .find(|(child, _)| *child == key.as_str())
            .and_then(|(_, parent)| parent.split_once(':'))
            .map(|(_, parent)| parent.to_owned());
        Ok(Some(row))
    }

    async fn store_cwd(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        NoParents.store_cwd(harness, native).await
    }

    async fn transcript(
        &self,
        harness: HarnessId,
        native: &str,
        window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        NoParents.transcript(harness, native, window).await
    }

    async fn rename(
        &self,
        harness: HarnessId,
        native: &str,
        title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        NoParents.rename(harness, native, title).await
    }

    async fn tool_output(
        &self,
        harness: HarnessId,
        native: &str,
        tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        NoParents.tool_output(harness, native, tool_call_id).await
    }
}

pub async fn a_child_s_notes_land_on_its_filed_ancestor_s_topic(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place_as(PARENT_KEY, &acmekit).await?;
    let note = "Invoices go out monthly.";
    stores
        .route(job, vec![patch("acmekit-billing", "Billing", note)?])
        .await?;
    stores.router().apply_deferred().await?;
    same(
        "the child's note on its filed ancestor's topic",
        stores.notes(&acmekit).await?,
        vec![("Billing".to_owned(), note.to_owned())],
    )?;
    Ok(())
}

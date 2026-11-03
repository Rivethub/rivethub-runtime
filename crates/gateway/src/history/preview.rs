use std::collections::HashMap;

use chrono::DateTime;
use kernel::preview::Preview;
use kernel::transcript::TranscriptRead;
use parking_lot::RwLock;
use wire::session_id::SessionId;

use super::{History, Resolved};
use crate::Gateway;
use crate::error::ApiError;

const CACHED: usize = 2048;

#[derive(Debug, Default)]
pub(crate) struct Previews {
    said: RwLock<HashMap<SessionId, (i64, Option<Preview>)>>,
}

impl History {
    pub(crate) async fn preview(
        &self,
        gateway: &Gateway,
        resolved: &Resolved,
    ) -> Result<Option<Preview>, ApiError> {
        let live = gateway
            .terminal()
            .and_then(|service| service.live_preview(&resolved.native));
        if live.is_some() {
            return Ok(live);
        }
        let Some(lister) = gateway.session_lister() else {
            return Ok(None);
        };
        let Some(row) = lister
            .describe(resolved.harness, &resolved.native, None)
            .await?
        else {
            return Ok(None);
        };
        if let Some((_, cached)) = self
            .previews
            .said
            .read()
            .get(&resolved.session)
            .filter(|(updated, _)| *updated == row.updated_at)
        {
            return Ok(cached.clone());
        }
        let updated = DateTime::from_timestamp_millis(row.updated_at).ok_or_else(|| {
            ApiError::Unprocessable(format!(
                "{} has an unreadable update time",
                resolved.session
            ))
        })?;
        let said = match lister
            .transcript(
                resolved.harness,
                &resolved.native,
                kernel::transcript::TranscriptWindow::Latest,
            )
            .await?
        {
            TranscriptRead::Read(read) => Preview::from_transcript(&read.turns, updated),
            TranscriptRead::NoSource => None,
        };
        let mut cache = self.previews.said.write();
        if cache.len() >= CACHED {
            cache.clear();
        }
        cache.insert(resolved.session.clone(), (row.updated_at, said.clone()));
        drop(cache);
        Ok(said)
    }
}

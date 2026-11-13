use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use wire::capture::CaptureBatchRequest;
use wire::harness::HarnessId;

use crate::hook::file::FileCapture;
use crate::hook::file::batch as file_batch;
use crate::hook::store::StoreCapture;
use crate::hook::store::StoreHarness;
use crate::listing::StoreRoots;
use crate::listing::StoreSessionLister;
use crate::listing::store_files;
use crate::readers::blocking;
use crate::writer::CaptureTransport;
use crate::writer::CaptureWriter;
use crate::writer::PostError;
use crate::writer::WriteError;

pub const HARNESSES: [HarnessId; 5] = [
    HarnessId::ClaudeCode,
    HarnessId::GrokBuild,
    HarnessId::Opencode,
    HarnessId::Codex,
    HarnessId::Dsh,
];

const EVENT: &str = "Backfill";

const EVERY_SESSION: usize = u32::MAX as usize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub session: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessTotals {
    pub harness: HarnessId,
    pub sessions: u64,
    pub messages: u64,
    pub added: u64,
    pub failures: Vec<Failure>,
}

impl HarnessTotals {
    fn new(harness: HarnessId) -> Self {
        Self {
            harness,
            sessions: 0,
            messages: 0,
            added: 0,
            failures: Vec::new(),
        }
    }

    fn fail(&mut self, session: &str, error: &impl ToString) {
        self.failures.push(Failure {
            session: session.to_owned(),
            error: error.to_string(),
        });
    }
}

type Built = Result<Option<CaptureBatchRequest>, String>;

#[derive(Debug)]
pub struct Backfill<T: CaptureTransport> {
    writer: CaptureWriter<T>,
    roots: StoreRoots,
}

impl<T: CaptureTransport> Backfill<T> {
    #[must_use]
    pub fn new(writer: CaptureWriter<T>, roots: StoreRoots) -> Self {
        Self { writer, roots }
    }

    pub async fn run(
        &self,
        harnesses: &[HarnessId],
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Vec<HarnessTotals> {
        let mut all = Vec::new();
        for harness in HARNESSES.into_iter().filter(|id| harnesses.contains(id)) {
            let mut totals = HarnessTotals::new(harness);
            let reachable = match harness {
                HarnessId::ClaudeCode => self.claude(&mut totals, dry_run, now).await,
                HarnessId::GrokBuild | HarnessId::Opencode | HarnessId::Codex | HarnessId::Dsh => {
                    self.store(&mut totals, dry_run, now).await
                }
                HarnessId::KimiCode
                | HarnessId::Hermes
                | HarnessId::Omp
                | HarnessId::Cursor
                | HarnessId::Cowork => true,
            };
            all.push(totals);
            if !reachable {
                break;
            }
        }
        all
    }

    async fn claude(&self, totals: &mut HarnessTotals, dry_run: bool, now: DateTime<Utc>) -> bool {
        let Some(root) = self.roots.claude.clone() else {
            return true;
        };
        let files = match blocking(move || store_files(&root)).await {
            Ok(files) => files,
            Err(error) => {
                totals.fail("store", &error);
                return true;
            }
        };
        for file in &files {
            let capture = FileCapture {
                file,
                last_event: EVENT,
                finalize: false,
                cwd: None,
                agent_type: None,
            };
            let built = file_batch(&capture, now)
                .await
                .map_err(|error| error.to_string());
            if !self.deliver(totals, &file.id, built, dry_run, now).await {
                return false;
            }
        }
        true
    }

    async fn store(&self, totals: &mut HarnessTotals, dry_run: bool, now: DateTime<Utc>) -> bool {
        let harness = totals.harness;
        let Some(kind) = store_harness(harness) else {
            return true;
        };
        let Some(capture) = StoreCapture::new(kind, &self.roots) else {
            return true;
        };
        let lister = StoreSessionLister::new(self.roots.clone());
        let rows = match lister.harness(harness, EVERY_SESSION).await {
            Ok(rows) => rows,
            Err(error) => {
                totals.fail("store", &error);
                return true;
            }
        };
        for row in rows {
            let built = capture
                .batch(&row.id, EVENT, row.cwd.clone(), false, now)
                .await;
            if !self.deliver(totals, &row.id, built, dry_run, now).await {
                return false;
            }
        }
        true
    }

    async fn deliver(
        &self,
        totals: &mut HarnessTotals,
        session: &str,
        built: Built,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> bool {
        totals.sessions += 1;
        let batch = match built {
            Ok(Some(batch)) => batch,
            Ok(None) => return true,
            Err(error) => {
                totals.fail(session, &error);
                return true;
            }
        };
        totals.messages += batch.messages.len() as u64;
        if dry_run {
            return true;
        }
        match self.writer.write(&batch, now).await {
            Ok(written) => {
                totals.added += written.inserted;
                true
            }
            Err(error) => {
                let reachable = rejected_alone(&error);
                totals.fail(session, &error);
                reachable
            }
        }
    }
}

fn store_harness(harness: HarnessId) -> Option<StoreHarness> {
    match harness {
        HarnessId::GrokBuild => Some(StoreHarness::GrokBuild),
        HarnessId::Opencode => Some(StoreHarness::Opencode),
        HarnessId::Codex => Some(StoreHarness::Codex),
        HarnessId::Dsh => Some(StoreHarness::Dsh),
        HarnessId::ClaudeCode
        | HarnessId::KimiCode
        | HarnessId::Hermes
        | HarnessId::Omp
        | HarnessId::Cursor
        | HarnessId::Cowork => None,
    }
}

fn rejected_alone(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::Post(PostError::Client { .. })
            | WriteError::ChunkTooLarge
            | WriteError::Json(_)
    )
}

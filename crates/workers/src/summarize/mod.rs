pub(crate) mod conversion;
pub(crate) mod index;
pub(crate) mod live;
mod llm;
mod settings;
mod sweep;
mod title;
pub(crate) mod top_down;

use kernel::memory::SummaryStore;
use kernel::model::ModelProvider;
use kernel::queue::JobQueue;
use tokio_util::sync::CancellationToken;

pub use live::{LiveEnv, LiveRequest, handle as handle_summarize};
pub(crate) use llm::llm_error;
pub use settings::SummarySettings;
pub use sweep::handle_reap;
pub use title::Named;
pub use title::TITLE_SYSTEM_PROMPT;
pub use title::TitleCall;
pub(crate) use title::handle_enqueue_openings;
pub(crate) use title::handle_opening_title;

#[expect(missing_debug_implementations)]
pub struct SummarizeEnv<'a> {
    pub store: &'a dyn SummaryStore,
    pub provider: &'a dyn ModelProvider,
    pub queue: &'a dyn JobQueue,
    pub settings: &'a SummarySettings,
    pub now: Timestamp,
    pub cancel: &'a CancellationToken,
}

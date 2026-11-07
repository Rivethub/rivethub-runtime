pub mod agent;
pub mod agent_presets;
pub mod ca;
pub mod cleanup;
pub mod facts;
pub mod filing;
pub mod flagged;
pub mod fork;
pub mod harness;
pub mod hub_state;
pub mod memory;
pub mod memory_core;
pub mod model;
pub mod owner;
pub mod plugins;
pub mod preview;
pub mod queue;
pub mod shutdown;
pub mod tasks;
pub mod tenancy;
pub mod terminal;
pub mod tidy;
pub mod tool;
pub mod topics;
pub mod transcript;
pub mod wiki;

use std::path::PathBuf;
use values::SessionId;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    pub agent_id: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub session: Option<SessionId>,
}

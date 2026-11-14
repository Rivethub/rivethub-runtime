use std::path::Path;

use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;

use super::session_dirs;
use crate::readers::scan::read_optional;
use crate::records::SubagentLink;
use crate::store_path::Root;
use crate::store_path::session_component;

const SUBAGENTS: &str = "subagents";
const META: &str = "meta.json";

#[derive(Debug, Deserialize)]
struct SpawnMeta {
    parent_session_id: String,
    subagent_id: Option<String>,
    child_session_id: Option<String>,
    subagent_type: Option<String>,
    description: Option<String>,
}

pub(crate) fn link(
    store: &Path,
    native_id: &str,
) -> Result<Option<SubagentLink>, TranscriptReaderError> {
    session_component(native_id)?;
    let root = Root::open(store)?;
    for (parent, dir) in session_dirs(&root)? {
        if parent == native_id {
            continue;
        }
        let path = dir.join(SUBAGENTS).join(native_id).join(META);
        let Some(text) = read_optional(&root, &path)? else {
            continue;
        };
        let meta: SpawnMeta =
            serde_json::from_str(&text).map_err(|error| TranscriptReaderError::Read {
                message: format!(
                    "grok subagent meta {} is not valid: {error}",
                    path.display()
                ),
                source: Box::new(error),
            })?;
        let child = meta.child_session_id.as_deref().unwrap_or(native_id);
        if child != native_id || meta.parent_session_id == native_id {
            continue;
        }
        return Ok(Some(SubagentLink {
            parent_session_id: meta.parent_session_id,
            agent_id: meta.subagent_id.unwrap_or_else(|| native_id.to_owned()),
            agent_type: meta.subagent_type,
            description: meta.description,
            tool_use_id: None,
        }));
    }
    Ok(None)
}

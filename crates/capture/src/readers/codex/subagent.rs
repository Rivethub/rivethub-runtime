use std::path::Path;

use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use serde_with::DefaultOnError;
use serde_with::serde_as;

use super::rollout_files;
use crate::readers::scan::read_optional;
use crate::records::SubagentLink;
use crate::store_path::Root;
use crate::store_path::session_component;

#[derive(Debug, Deserialize)]
struct MetaLine {
    #[serde(rename = "type")]
    kind: String,
    payload: Option<SessionMeta>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct SessionMeta {
    id: String,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Subagent(Spawned),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Spawned {
    ThreadSpawn(ThreadSpawn),
}

#[derive(Debug, Deserialize)]
struct ThreadSpawn {
    parent_thread_id: String,
    #[serde(default, alias = "agent_type")]
    agent_role: Option<String>,
}

pub(crate) fn link(
    store: &Path,
    native_id: &str,
) -> Result<Option<SubagentLink>, TranscriptReaderError> {
    session_component(native_id)?;
    let root = Root::open(store)?;
    let Some((_, path)) = rollout_files(&root)?
        .into_iter()
        .find(|(id, _)| id == native_id)
    else {
        return Ok(None);
    };
    let Some(text) = read_optional(&root, &path)? else {
        return Ok(None);
    };
    for raw in text.lines().filter(|raw| raw.contains("\"session_meta\"")) {
        let line: MetaLine =
            serde_json::from_str(raw).map_err(|error| TranscriptReaderError::Read {
                message: format!(
                    "codex session_meta in {} is not valid: {error}",
                    path.display()
                ),
                source: Box::new(error),
            })?;
        let Some(meta) = line.payload.filter(|_| line.kind == "session_meta") else {
            continue;
        };
        let Some(Source::Subagent(Spawned::ThreadSpawn(spawn))) = meta.source else {
            return Ok(None);
        };
        if spawn.parent_thread_id.is_empty() || spawn.parent_thread_id == meta.id {
            return Ok(None);
        }
        return Ok(Some(SubagentLink {
            parent_session_id: spawn.parent_thread_id,
            agent_id: meta.id,
            agent_type: spawn.agent_role.filter(|role| !role.is_empty()),
            description: None,
            tool_use_id: None,
        }));
    }
    Ok(None)
}

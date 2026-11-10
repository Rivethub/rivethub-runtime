mod files;
mod find;
mod flow;
mod import;

use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::share::SessionShare;
use kernel::owner::Owner;
use rmcp::model::ToolAnnotations;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

pub use files::BundleName;
pub use files::NameError;
pub use files::PathRefusal;
pub use files::ReadPath;
pub use files::ShareFiles;
pub use files::refuse_existing;
pub(crate) use find::find;
pub(crate) use flow::export;
pub(crate) use flow::sanitize;
pub(crate) use import::import;

const MAX_IDS: usize = 50;

#[derive(Debug)]
pub struct Sessions {
    pub share: Arc<dyn SessionShare>,
    pub files: ShareFiles,
    pub owner: Owner,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<Uuid>")]
pub struct ConversationIds(Vec<Uuid>);

impl ConversationIds {
    pub fn as_slice(&self) -> &[Uuid] {
        &self.0
    }
}

impl TryFrom<Vec<Uuid>> for ConversationIds {
    type Error = NameError;

    fn try_from(ids: Vec<Uuid>) -> Result<Self, Self::Error> {
        let mut unique: Vec<Uuid> = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        if unique.is_empty() || unique.len() > MAX_IDS {
            return Err(NameError(format!(
                "conversation_ids names {} distinct ids; name from 1 to {MAX_IDS}",
                unique.len()
            )));
        }
        Ok(Self(unique))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Find {
    #[schemars(description = "What the sessions were about: a question, a prompt or keywords")]
    pub(crate) query: String,
    #[schemars(description = "Only sessions with a matching message after this time (ISO UTC timestamp)", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    pub(crate) since: Option<DateTime<Utc>>,
    #[schemars(description = "Only sessions with a matching message before this time (ISO UTC timestamp)", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    pub(crate) before: Option<DateTime<Utc>>,
    #[schemars(description = "Only sessions of this agent (opus, codex, etc.)", transform = crate::catalog::optional_non_null)]
    pub(crate) agent: Option<String>,
    #[schemars(description = "Only sessions whose conversation carries this accepted key:value tag (e.g. project:acmeapp)", transform = crate::catalog::optional_non_null)]
    pub(crate) tag: Option<String>,
    #[schemars(description = "Max sessions (1-25, default 10)", range(min = 1, max = 25), transform = crate::catalog::optional_non_null)]
    pub(crate) limit: Option<u32>,
}

impl crate::catalog::ToolInput for Find {
    const NAME: &'static str = "session_find";
    const DESCRIPTION: &'static str = "Find this node's captured sessions related to a prompt, best match first. Each entry gives the conversation id that session_export takes, the title, agent, first and last date, message count, best score and up to three matching excerpts. Read-only.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            Some(false),
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    #[schemars(
        description = "Conversation ids from session_find (1-50 UUIDs)",
        with = "Vec<String>",
        length(min = 1, max = 50)
    )]
    pub(crate) conversation_ids: ConversationIds,
    #[schemars(
        description = "Bundle file name in the share directory, such as work.json (letters, digits, '.', '_' and '-', ending in .json); an existing file is never replaced",
        with = "String"
    )]
    pub(crate) out: BundleName,
}

impl crate::catalog::ToolInput for Export {
    const NAME: &'static str = "session_export";
    const DESCRIPTION: &'static str = "Write the named sessions of this node's owner to a raw session bundle in the share directory and return its absolute path. The bundle is not sanitized: run session_sanitize on it before it leaves this machine. An unknown id is refused by name and nothing is written.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(false),
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sanitize {
    #[serde(rename = "in")]
    #[schemars(
        rename = "in",
        description = "The raw bundle: a file name in the share directory, or an absolute path under your home directory",
        with = "String"
    )]
    pub(crate) input: ReadPath,
    #[schemars(
        description = "Sanitized bundle file name in the share directory, such as work.clean.json; the report and the local review file are written beside it and no existing file is replaced",
        with = "String"
    )]
    pub(crate) out: BundleName,
    #[schemars(description = "Mapping file (.toml): a file name in the share directory, or an absolute path under your home directory. Default: memory.sanitize.mapping from the config", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    pub(crate) mapping: Option<ReadPath>,
    #[schemars(description = "Also run the decision-model review configured at memory.sanitize.decide (default false)", transform = crate::catalog::optional_non_null)]
    pub(crate) decide: Option<bool>,
}

impl crate::catalog::ToolInput for Sanitize {
    const NAME: &'static str = "session_sanitize";
    const DESCRIPTION: &'static str = "Sanitize a raw session bundle into a new bundle in the share directory, with a report and, after a decision-model review, a local review file beside it. Returns totals, the absolute paths, changes by rule and, with decide, each flag's number, score, place, report line and length. It never returns flagged text. Share only the bundle; the report and the local review file stay on this machine.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(false),
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Import {
    #[schemars(
        description = "The sanitized bundle: a file name in the share directory, or an absolute path under your home directory",
        with = "String"
    )]
    pub(crate) path: ReadPath,
    #[schemars(description = "List the sessions and the verdict without writing (default false)", transform = crate::catalog::optional_non_null)]
    pub(crate) dry_run: Option<bool>,
}

impl crate::catalog::ToolInput for Import {
    const NAME: &'static str = "session_import";
    const DESCRIPTION: &'static str = "Import a sanitized session bundle into this node's own memory, keeping each session's original dates and crediting its author. Refuses a raw bundle, a bundle the secret scan still matches and a bundle with open withheld flags. dry_run lists the sessions and the verdict and writes nothing. Importing the same bundle again adds nothing.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(false),
            Some(false),
            Some(true),
            Some(false),
        ))
    }
}

fn noun<T>(count: T, one: &'static str, many: &'static str) -> String
where
    T: std::fmt::Display + PartialEq + From<u8> + Copy,
{
    let word = if count == T::from(1) { one } else { many };
    format!("{count} {word}")
}

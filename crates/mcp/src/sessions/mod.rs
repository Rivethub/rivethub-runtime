mod chosen;
mod files;
mod find;
mod flow;
mod import;
mod share;

use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::share::SessionShare;
use kernel::owner::Owner;
use rmcp::model::ToolAnnotations;
use schemars::JsonSchema;
use serde::Deserialize;

use chosen::Chosen;
use chosen::ChosenSession;
pub use files::BundleName;
pub use files::NameError;
pub use files::PathRefusal;
pub use files::ReadPath;
pub use files::ShareFiles;
pub use files::refuse_existing;
pub(crate) use find::find;
pub(crate) use flow::sanitize;
pub(crate) use import::import;
pub(crate) use share::share;

#[derive(Debug)]
pub struct Sessions {
    pub share: Arc<dyn SessionShare>,
    pub files: ShareFiles,
    pub owner: Owner,
    pub node: String,
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
    const DESCRIPTION: &'static str = "Find this node's captured sessions related to a prompt, best match first. Each entry gives the conversation id that session_sanitize takes, the title, agent, first and last date, message count, best score and up to three matching excerpts. Read-only.";

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
pub struct Sanitize {
    #[schemars(
        description = "The sessions to share, each from session_find with your decision; leave out the sessions you deny, which never leave the store (1-50, each id once)",
        with = "Vec<ChosenSession>",
        length(min = 1, max = 50)
    )]
    pub(crate) sessions: Chosen,
    #[schemars(
        description = "Sanitized bundle file name in the share directory, such as work.clean.json; the report and the local review file are written beside it and no existing file is replaced",
        with = "String"
    )]
    pub(crate) out: BundleName,
    #[schemars(description = "Mapping file (.toml): a file name in the share directory, or an absolute path under your home directory. Default: memory.sanitize.mapping from the config", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    pub(crate) mapping: Option<ReadPath>,
    #[schemars(description = "Also run the decision-model review configured at memory.sanitize.decide (default false)", transform = crate::catalog::optional_non_null)]
    pub(crate) decide: Option<bool>,
    #[schemars(description = "With decide, withhold every flagged chunk until it is marked, whatever memory.sanitize.decide.block says (default: the config)", transform = crate::catalog::optional_non_null)]
    pub(crate) block: Option<bool>,
}

impl crate::catalog::ToolInput for Sanitize {
    const NAME: &'static str = "session_sanitize";
    const DESCRIPTION: &'static str = "Sanitize the sessions you chose into a bundle in the share directory, reading them straight from this node's store, so no raw bundle is ever written. Give each session to share a decision: approve, or redact with terms to redact in that session only and/or drop_tool_outputs to replace its tool outputs; sessions you leave out are denied. A report and, after a decision-model review, a local review file are written beside the bundle. Returns totals, the absolute paths, changes by rule and, with decide, each flag's number, score, place, report line and length. It never returns flagged text. Share only the bundle; the report and the local review file stay on this machine.";

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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Share {
    #[schemars(
        description = "The sanitized bundle to share: a file name in the share directory, or an absolute path under your home directory",
        with = "String"
    )]
    pub(crate) bundle: ReadPath,
}

impl crate::catalog::ToolInput for Share {
    const NAME: &'static str = "session_share";
    const DESCRIPTION: &'static str = "Seal a sanitized bundle (encrypted, signed with this node's key) and upload the ciphertext to the RivetHub Cloud drop box, after the user confirms in a prompt that shows the session count, titles, author, signature and size. Needs a client that supports elicitation; otherwise it refuses and the user runs rh memory share. Returns the one-time share code for the user to send privately; no tool sends, posts or stores the code, and there is no receive tool.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(true),
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

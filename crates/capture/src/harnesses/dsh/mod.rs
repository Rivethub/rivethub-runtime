mod patch;
mod plan;
mod profile;
mod status;

pub(super) use plan::plan;
pub(super) use status::capture_status;
pub(super) use status::recall_status;

use std::path::Path;
use std::path::PathBuf;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::SetupError;

use super::locate::Locator;
use super::registry::Base;
use super::registry::Location;

const MARKER: &str = "capture hook --harness dsh";
const BEGIN: &str = "# rivethub-dsh-begin\n";
const END: &str = "# rivethub-dsh-end\n";
const EMPTY: &str = "# rivethub-dsh-empty\n";
const PATCH_NAME: &str = "cordis.patch.yml";
const PLUGIN_REL: &str = "plugins/rivethub-capture.mjs";
const PLUGIN_NAME: &str = "./plugins/rivethub-capture.mjs";
const CAPTURE_ID: &str = "rivethub-capture";
const RECALL_ID: &str = "rivethub-recall";
const LEGACY_ENTRY: &str = "rivetos";
const NO_BINARY: &str = "rh cannot find the dsh binary on PATH, so it cannot locate @deepseek-ai/dsh-mcp-client for a recall row";
const NO_PACKAGE: &str = "the dsh install next to the dsh binary has no @deepseek-ai/dsh-mcp-client, so rh cannot add a recall row";
const FLOW: &str = "cordis.patch.yml is a flow-style YAML array rh will not rewrite";
const NOT_ARRAY: &str = "cordis.patch.yml is not a YAML array rh can extend";
const UNBALANCED: &str = "cordis.patch.yml has unbalanced rivethub markers";
const NO_NEWLINE: &str = "cordis.patch.yml does not end with a newline";
const FOREIGN_PLUGIN: &str = "plugins/rivethub-capture.mjs exists and is not rh's plug-in";
const NAME_TAKEN: &str = "an entry named rivethub-recall already runs something else";
const WALK_LIMIT: usize = 8;

#[derive(Debug)]
pub(super) struct Coverage {
    pub(super) installed: bool,
    pub(super) detail: String,
}

#[derive(Debug)]
struct Paths {
    root: PathBuf,
    patch: PathBuf,
    plugin: PathBuf,
}

fn paths(locator: &Locator) -> Result<Paths, SetupError> {
    let root = locator
        .target(Location {
            base: Base::Dsh,
            relative: "",
        })
        .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?;
    Ok(Paths {
        patch: root.join(PATCH_NAME),
        plugin: root.join(PLUGIN_REL),
        root,
    })
}

fn unsupported(reason: &'static str) -> SetupError {
    SetupError::Unsupported {
        id: "dsh".to_owned(),
        part: ConnectPart::Recall,
        reason,
    }
}

fn config(path: &Path, message: impl Into<String>) -> SetupError {
    SetupError::Config {
        path: path.display().to_string(),
        message: message.into(),
    }
}

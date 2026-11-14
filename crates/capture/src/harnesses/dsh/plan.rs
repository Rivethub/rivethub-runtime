use std::fs;
use std::path::Path;
use std::path::PathBuf;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::SetupError;

use super::super::dsh_plugin;
use super::super::locate::Locator;
use super::super::plan::FileEdit;
use super::super::plan::read_optional;
use super::FOREIGN_PLUGIN;
use super::MARKER;
use super::NAME_TAKEN;
use super::NO_BINARY;
use super::NO_PACKAGE;
use super::Paths;
use super::WALK_LIMIT;
use super::config;
use super::patch::capture_row;
use super::patch::compose;
use super::patch::owned_rows;
use super::patch::recall_row;
use super::patch::region_of;
use super::patch::rows_after_removal;
use super::patch::splice_rows;
use super::patch::validate_array;
use super::paths;
use super::profile::Seen;
use super::profile::seen_in;
use super::unsupported;

enum Client {
    Ready(PathBuf),
    NoBinary,
    NoPackage,
}

pub(in super::super) fn plan(
    locator: &Locator,
    rh: &Path,
    parts: &[ConnectPart],
    connecting: bool,
) -> Result<(Vec<FileEdit>, Option<SetupError>), SetupError> {
    let paths = paths(locator)?;
    if connecting {
        connect(locator, rh, parts, &paths)
    } else {
        Ok((disconnect(parts, &paths)?, None))
    }
}

fn connect(
    locator: &Locator,
    rh: &Path,
    parts: &[ConnectPart],
    paths: &Paths,
) -> Result<(Vec<FileEdit>, Option<SetupError>), SetupError> {
    let want_capture = parts.contains(&ConnectPart::Capture);
    let want_recall = parts.contains(&ConnectPart::Recall);
    let before = read_optional(&paths.patch)?;
    let text = before.clone().unwrap_or_default();
    let region = region_of(&paths.patch, &text)?;
    let owned = owned_rows(&paths.patch, region.as_ref())?;
    let seen = seen_in(&paths.root, &text).map_err(|error| config(&paths.patch, error))?;
    let client = locate_client(locator);
    let decision = decide(
        want_capture,
        want_recall,
        parts,
        &seen,
        &client,
        &paths.patch,
    )?;
    if decision.write_plugin {
        refuse_foreign_plugin(&paths.plugin)?;
    }
    let mut rows = owned;
    if decision.add_capture {
        rows.push(capture_row());
    }
    if let Some(client_path) = &decision.client {
        rows.push(recall_row(client_path, rh));
    }
    let mut edits = Vec::new();
    if decision.write_plugin {
        push_edit(&mut edits, plugin_edit(&paths.plugin, rh)?);
    }
    if decision.add_capture || decision.client.is_some() {
        let after = splice_rows(&paths.patch, &text, region.as_ref(), &rows)?;
        push_edit(
            &mut edits,
            FileEdit {
                path: paths.patch.clone(),
                before,
                after: Some(after),
            },
        );
    }
    Ok((edits, decision.skipped))
}

fn disconnect(parts: &[ConnectPart], paths: &Paths) -> Result<Vec<FileEdit>, SetupError> {
    let want_capture = parts.contains(&ConnectPart::Capture);
    let want_recall = parts.contains(&ConnectPart::Recall);
    let before = read_optional(&paths.patch)?;
    let mut edits = Vec::new();
    if let Some(text) = &before {
        let region = region_of(&paths.patch, text)?;
        if let Some(region) = region.as_ref() {
            let rows = rows_after_removal(&paths.patch, region, want_capture, want_recall)?;
            let after = compose(region, &rows);
            if let Some(kept) = &after {
                validate_array(&paths.patch, kept)?;
            }
            push_edit(
                &mut edits,
                FileEdit {
                    path: paths.patch.clone(),
                    before: Some(text.clone()),
                    after,
                },
            );
        }
    }
    if want_capture && let Some(edit) = plugin_removal(&paths.plugin)? {
        push_edit(&mut edits, edit);
    }
    Ok(edits)
}

#[derive(Debug)]
struct Decision {
    write_plugin: bool,
    add_capture: bool,
    client: Option<PathBuf>,
    skipped: Option<SetupError>,
}

fn decide(
    want_capture: bool,
    want_recall: bool,
    parts: &[ConnectPart],
    seen: &Seen,
    client: &Client,
    patch: &Path,
) -> Result<Decision, SetupError> {
    let capture_open = want_capture && !seen.capture.marker;
    let mut skipped = None;
    let mut recall_path = None;
    if want_recall && !seen.recall.runs_rh {
        match client {
            Client::Ready(found) => {
                if seen.recall.taken {
                    return Err(config(patch, NAME_TAKEN));
                }
                recall_path = Some(found.clone());
            }
            Client::NoBinary => skipped = Some(unsupported(NO_BINARY)),
            Client::NoPackage => skipped = Some(unsupported(NO_PACKAGE)),
        }
    }
    if skipped.is_some() && parts == [ConnectPart::Recall] {
        return Err(unsupported(match client {
            Client::NoPackage => NO_PACKAGE,
            Client::NoBinary | Client::Ready(_) => NO_BINARY,
        }));
    }
    Ok(Decision {
        write_plugin: capture_open,
        add_capture: capture_open && !seen.capture.named,
        client: recall_path,
        skipped,
    })
}

fn plugin_edit(path: &Path, rh: &Path) -> Result<FileEdit, SetupError> {
    let before = read_optional(path)?;
    let quoted = shlex::try_quote(&rh.to_string_lossy())
        .map_err(|error| config(rh, error.to_string()))?
        .into_owned();
    let command = format!("printf \"%s\" \"$1\" | exec {quoted} {MARKER}");
    let literal = serde_json::to_string(&command).map_err(|error| config(rh, error.to_string()))?;
    Ok(FileEdit {
        path: path.to_path_buf(),
        before,
        after: Some(dsh_plugin::TEMPLATE.replace("'__COMMAND__'", &literal)),
    })
}

fn plugin_removal(path: &Path) -> Result<Option<FileEdit>, SetupError> {
    let Some(before) = read_optional(path)? else {
        return Ok(None);
    };
    if !before.contains(MARKER) {
        return Ok(None);
    }
    Ok(Some(FileEdit {
        path: path.to_path_buf(),
        before: Some(before),
        after: None,
    }))
}

fn refuse_foreign_plugin(path: &Path) -> Result<(), SetupError> {
    match read_optional(path)? {
        Some(text) if !text.contains(MARKER) => Err(config(path, FOREIGN_PLUGIN)),
        Some(_) | None => Ok(()),
    }
}

fn push_edit(edits: &mut Vec<FileEdit>, edit: FileEdit) {
    if edit.is_change() && !(edit.after.is_none() && edit.before.is_none()) {
        edits.push(edit);
    }
}

fn locate_client(locator: &Locator) -> Client {
    let Some(binary) = locator.which("dsh") else {
        return Client::NoBinary;
    };
    find_client(&binary).map_or(Client::NoPackage, Client::Ready)
}

fn find_client(binary: &Path) -> Option<PathBuf> {
    let canonical = canonical_or_given(binary);
    let mut starts = Vec::new();
    if let Some(parent) = canonical.parent() {
        starts.push(parent.to_path_buf());
    }
    if let Some(parent) = binary.parent()
        && starts.first() != Some(&parent.to_path_buf())
    {
        starts.push(parent.to_path_buf());
    }
    for start in starts {
        if let Some(found) = walk_for_client(&start) {
            return Some(found);
        }
    }
    None
}

fn walk_for_client(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..WALK_LIMIT {
        let nested = dir.join("node_modules/@deepseek-ai/dsh-mcp-client/lib/index.js");
        if nested.is_file() {
            return Some(canonical_or_given(&nested));
        }
        let sibling = dir.join("dsh-mcp-client/lib/index.js");
        if sibling.is_file() {
            return Some(canonical_or_given(&sibling));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn canonical_or_given(path: &Path) -> PathBuf {
    match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "canonicalize");
            path.to_path_buf()
        }
    }
}

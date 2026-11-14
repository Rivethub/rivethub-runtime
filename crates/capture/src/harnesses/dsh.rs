use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::SetupError;
use serde::Deserialize;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use serde_yaml_ng::Value;

use super::dsh_plugin;
use super::entries;
use super::locate::Locator;
use super::plan::FileEdit;
use super::plan::read_optional;
use super::recall::ENTRY_NAME;
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

#[derive(Debug, Default)]
struct CaptureScan {
    marker: bool,
    named: bool,
    missing: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct RecallScan {
    runs_rh: bool,
    taken: bool,
    server: Option<String>,
}

#[derive(Debug, Default)]
struct Seen {
    capture: CaptureScan,
    recall: RecallScan,
}

#[derive(Debug)]
struct Row {
    id: String,
    name: String,
    recall: Option<RecallFields>,
}

#[derive(Debug)]
struct RecallFields {
    server_name: Option<String>,
    command: String,
    args: Vec<String>,
}

#[derive(Debug)]
struct Region<'a> {
    prefix: &'a str,
    inner: &'a str,
    suffix: &'a str,
    empty_array: bool,
}

enum Client {
    Ready(PathBuf),
    NoBinary,
    NoPackage,
}

pub(super) fn plan(
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

pub(super) fn capture_status(locator: &Locator) -> Coverage {
    let paths = match paths(locator) {
        Ok(paths) => paths,
        Err(error) => {
            return Coverage {
                installed: false,
                detail: format!("no home directory to look in: {error}"),
            };
        }
    };
    let profiles = profile_names(&paths.root);
    match home_capture(&paths) {
        Home::Covers => Coverage {
            installed: true,
            detail: cover_detail(&paths.patch, &profiles),
        },
        Home::Missing(path) => Coverage {
            installed: false,
            detail: format!("home patch names a missing plug-in at {}", path.display()),
        },
        Home::Unread(error) => Coverage {
            installed: false,
            detail: format!("cannot read plug-ins: {error}"),
        },
        Home::Absent => Coverage {
            installed: false,
            detail: capture_gap(&paths, &profiles),
        },
    }
}

pub(super) fn recall_status(locator: &Locator) -> (RecallState, String) {
    let paths = match paths(locator) {
        Ok(paths) => paths,
        Err(error) => {
            return (
                RecallState::NotConnected,
                format!("no home directory to look in: {error}"),
            );
        }
    };
    let profiles = profile_names(&paths.root);
    match home_recall(&paths) {
        RecallHome::Connected(name) => (
            RecallState::Connected,
            format!(
                "{name} runs rh mcp in {} (every profile, including ones created later)",
                paths.patch.display()
            ),
        ),
        RecallHome::Elsewhere { name, invocation } => (
            RecallState::Elsewhere,
            format!(
                "{name} in {} runs {invocation}, not rh mcp",
                paths.patch.display()
            ),
        ),
        RecallHome::Unread(error) => (
            RecallState::NotConnected,
            format!("cannot read MCP config: {error}"),
        ),
        RecallHome::Absent => (RecallState::NotConnected, recall_gap(&paths, &profiles)),
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

fn rows_after_removal(
    patch: &Path,
    region: &Region<'_>,
    want_capture: bool,
    want_recall: bool,
) -> Result<Vec<Row>, SetupError> {
    let mut rows = owned_rows(patch, Some(region))?;
    if want_capture {
        rows.retain(|row| row.id != CAPTURE_ID && !names_our_plugin(&row.name));
    }
    if want_recall {
        rows.retain(|row| row.id != RECALL_ID && !row_runs_rh(row));
    }
    Ok(rows)
}

fn splice_rows(
    patch: &Path,
    text: &str,
    region: Option<&Region<'_>>,
    rows: &[Row],
) -> Result<String, SetupError> {
    let block = render_block(rows);
    let after = if let Some(region) = region {
        compose_kept(region, &block)
    } else if text.trim().is_empty() {
        block
    } else {
        extend_patch(patch, text, &block)?
    };
    validate_array(patch, &after)?;
    Ok(after)
}

fn extend_patch(patch: &Path, text: &str, block: &str) -> Result<String, SetupError> {
    if !text.ends_with('\n') {
        return Err(config(patch, NO_NEWLINE));
    }
    if is_plain_empty_array(text) {
        return Ok(replace_empty_array(text, &format!("{EMPTY}{block}")));
    }
    if first_code(text).is_some_and(|line| line.starts_with('[')) {
        return Err(config(patch, FLOW));
    }
    serde_yaml_ng::from_str::<Vec<Value>>(text).map_err(|error| config(patch, not_array(error)))?;
    Ok(format!("{text}{block}"))
}

fn compose(region: &Region<'_>, rows: &[Row]) -> Option<String> {
    if rows.is_empty() {
        return empty_remainder(region);
    }
    Some(compose_kept(region, &render_block(rows)))
}

fn compose_kept(region: &Region<'_>, block: &str) -> String {
    let mut text = region.prefix.to_owned();
    if region.empty_array {
        text.push_str(EMPTY);
    }
    text.push_str(block);
    text.push_str(region.suffix);
    text
}

fn empty_remainder(region: &Region<'_>) -> Option<String> {
    if region.empty_array {
        return Some(format!("{}[]\n{}", region.prefix, region.suffix));
    }
    let text = format!("{}{}", region.prefix, region.suffix);
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn region_of<'a>(patch: &Path, text: &'a str) -> Result<Option<Region<'a>>, SetupError> {
    let begins = text.matches(BEGIN).count();
    let ends = text.matches(END).count();
    if begins == 0 && ends == 0 {
        return Ok(None);
    }
    if begins != 1 || ends != 1 {
        return Err(config(patch, UNBALANCED));
    }
    let Some(start) = text.find(BEGIN) else {
        return Err(config(patch, UNBALANCED));
    };
    let after_begin = start.saturating_add(BEGIN.len());
    let Some(end_at) = text.get(after_begin..).and_then(|rest| rest.find(END)) else {
        return Err(config(patch, UNBALANCED));
    };
    let inner_end = after_begin.saturating_add(end_at);
    let suffix_at = inner_end.saturating_add(END.len());
    let Some(inner) = text.get(after_begin..inner_end) else {
        return Err(config(patch, UNBALANCED));
    };
    let Some(suffix) = text.get(suffix_at..) else {
        return Err(config(patch, UNBALANCED));
    };
    let empty_at = start.checked_sub(EMPTY.len());
    let empty_array = empty_at
        .and_then(|at| text.get(at..start))
        .is_some_and(|prefix| prefix == EMPTY);
    let prefix_at = if empty_array {
        empty_at.unwrap_or(start)
    } else {
        start
    };
    let Some(prefix) = text.get(..prefix_at) else {
        return Err(config(patch, UNBALANCED));
    };
    Ok(Some(Region {
        prefix,
        inner,
        suffix,
        empty_array,
    }))
}

fn owned_rows(patch: &Path, region: Option<&Region<'_>>) -> Result<Vec<Row>, SetupError> {
    let Some(region) = region else {
        return Ok(Vec::new());
    };
    if region.inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let docs: Vec<Value> = serde_yaml_ng::from_str(region.inner).map_err(|error| {
        config(
            patch,
            format!("cordis.patch.yml markers do not hold a YAML array: {error}"),
        )
    })?;
    Ok(rows_of(&docs))
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

fn capture_row() -> Row {
    Row {
        id: CAPTURE_ID.to_owned(),
        name: PLUGIN_NAME.to_owned(),
        recall: None,
    }
}

fn recall_row(client: &Path, rh: &Path) -> Row {
    Row {
        id: RECALL_ID.to_owned(),
        name: client.display().to_string(),
        recall: Some(RecallFields {
            server_name: Some(ENTRY_NAME.to_owned()),
            command: rh.display().to_string(),
            args: vec!["mcp".to_owned()],
        }),
    }
}

fn render_block(rows: &[Row]) -> String {
    let mut text = format!("{BEGIN}- insert:\n");
    for row in rows {
        text.push_str(&render_row(row));
    }
    text.push_str(END);
    text
}

fn render_row(row: &Row) -> String {
    let head = format!(
        "    - id: {}\n      name: {}\n",
        plain_or_quoted(&row.id),
        yaml_single(&row.name)
    );
    let Some(fields) = &row.recall else {
        return head;
    };
    let mut args = String::new();
    for arg in &fields.args {
        args.push_str("          - ");
        args.push_str(&plain_or_quoted(arg));
        args.push('\n');
    }
    format!(
        "{head}      config:\n        serverName: {}\n        transport: stdio\n        command: {}\n        args:\n{args}        failOnStartupError: false\n",
        plain_or_quoted(fields.server_name.as_deref().unwrap_or(ENTRY_NAME)),
        yaml_single(&fields.command)
    )
}

fn rows_of(docs: &[Value]) -> Vec<Row> {
    let mut rows = Vec::new();
    for doc in docs {
        let Some(insert) = doc.get("insert").and_then(Value::as_sequence) else {
            continue;
        };
        for item in insert {
            if let Some(row) = row_from(item) {
                rows.push(row);
            }
        }
    }
    rows
}

fn row_from(item: &Value) -> Option<Row> {
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    let name = item.get("name").and_then(Value::as_str)?.to_owned();
    let recall = item
        .get("config")
        .filter(|config| config.is_mapping())
        .map(|config| RecallFields {
            server_name: config
                .get("serverName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            command: command_text(config.get("command")),
            args: arg_list(config.get("args")),
        });
    Some(Row { id, name, recall })
}

fn row_runs_rh(row: &Row) -> bool {
    let Some(fields) = &row.recall else {
        return false;
    };
    let mut text = fields.command.clone();
    for arg in &fields.args {
        text.push(' ');
        text.push_str(arg);
    }
    entries::runs_rh_mcp(&text)
}

fn seen_in(root: &Path, text: &str) -> Result<Seen, String> {
    let mut seen = Seen::default();
    if text.trim().is_empty() {
        return Ok(seen);
    }
    let docs: Vec<Value> = serde_yaml_ng::from_str(text).map_err(not_array)?;
    for row in rows_of(&docs) {
        note_row(root, &row, &mut seen);
    }
    Ok(seen)
}

fn note_row(root: &Path, row: &Row, seen: &mut Seen) {
    if names_our_plugin(&row.name) {
        seen.capture.named = true;
    }
    match resolved_plugin(root, &row.name) {
        Some(path) if contains_marker(&path) => seen.capture.marker = true,
        Some(path) if row.id == CAPTURE_ID || names_our_plugin(&row.name) => {
            if seen.capture.missing.is_none() && !path.exists() {
                seen.capture.missing = Some(path);
            }
        }
        Some(_) | None => {}
    }
    if row_runs_rh(row) {
        seen.recall.runs_rh = true;
        if seen.recall.server.is_none() {
            seen.recall.server = Some(row_server(row));
        }
        return;
    }
    if names_our_recall(row) {
        seen.recall.taken = true;
    }
}

fn names_our_recall(row: &Row) -> bool {
    row.id == RECALL_ID
        || row
            .recall
            .as_ref()
            .is_some_and(|fields| fields.server_name.as_deref().is_some_and(is_memory_name))
}

fn row_server(row: &Row) -> String {
    row.recall
        .as_ref()
        .and_then(|fields| fields.server_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| row.id.clone())
}

fn names_our_plugin(name: &str) -> bool {
    name == PLUGIN_NAME || name.ends_with("/plugins/rivethub-capture.mjs")
}

fn is_memory_name(name: &str) -> bool {
    name == ENTRY_NAME || name == LEGACY_ENTRY
}

enum Home {
    Covers,
    Missing(PathBuf),
    Unread(String),
    Absent,
}

enum RecallHome {
    Connected(String),
    Elsewhere { name: String, invocation: String },
    Unread(String),
    Absent,
}

fn home_capture(paths: &Paths) -> Home {
    match read_patch(&paths.patch) {
        Err(error) => Home::Unread(error),
        Ok(text) => match seen_in(&paths.root, &text) {
            Err(error) => Home::Unread(error),
            Ok(seen) if seen.capture.marker => Home::Covers,
            Ok(seen) => seen.capture.missing.map_or(Home::Absent, Home::Missing),
        },
    }
}

fn home_recall(paths: &Paths) -> RecallHome {
    match read_patch(&paths.patch) {
        Err(error) => RecallHome::Unread(error),
        Ok(text) => match seen_in(&paths.root, &text) {
            Err(error) => RecallHome::Unread(error),
            Ok(seen) if seen.recall.runs_rh => {
                let name = match seen.recall.server {
                    Some(name) => name,
                    None => ENTRY_NAME.to_owned(),
                };
                RecallHome::Connected(name)
            }
            Ok(seen) if seen.recall.taken => recall_elsewhere(&text),
            Ok(_) => RecallHome::Absent,
        },
    }
}

fn recall_elsewhere(text: &str) -> RecallHome {
    let docs = match serde_yaml_ng::from_str::<Vec<Value>>(text) {
        Ok(docs) => docs,
        Err(error) => return RecallHome::Unread(error.to_string()),
    };
    for row in rows_of(&docs) {
        if row_runs_rh(&row) || !names_our_recall(&row) {
            continue;
        }
        let invocation = row
            .recall
            .as_ref()
            .map(|fields| {
                let mut text = fields.command.clone();
                for arg in &fields.args {
                    text.push(' ');
                    text.push_str(arg);
                }
                text
            })
            .unwrap_or_default();
        return RecallHome::Elsewhere {
            name: row_server(&row),
            invocation,
        };
    }
    RecallHome::Absent
}

fn capture_gap(paths: &Paths, profiles: &[String]) -> String {
    gap(
        &format!("no rh plug-in in {}", paths.patch.display()),
        "plug-in linked in ",
        paths,
        profiles,
        legacy_capture,
    )
}

fn recall_gap(paths: &Paths, profiles: &[String]) -> String {
    gap(
        &format!("no rh mcp entry in {}", paths.patch.display()),
        "rh mcp in ",
        paths,
        profiles,
        legacy_recall,
    )
}

fn gap(
    none: &str,
    linked: &str,
    paths: &Paths,
    profiles: &[String],
    wired: fn(&Path, &str) -> bool,
) -> String {
    if profiles.is_empty() {
        return format!("{none}; no profile directory yet");
    }
    let (yes, no) = split_profiles(&paths.root, profiles, wired);
    if yes.is_empty() {
        format!("{none}; not wired: {}", no.join(", "))
    } else if no.is_empty() {
        format!(
            "{linked}{}; profiles created later are not covered",
            yes.join(", ")
        )
    } else {
        format!("{linked}{}; not wired: {}", yes.join(", "), no.join(", "))
    }
}

fn cover_detail(patch: &Path, profiles: &[String]) -> String {
    if profiles.is_empty() {
        format!(
            "home patch {} covers every profile, including profiles created later",
            patch.display()
        )
    } else {
        format!(
            "home patch {} covers every profile ({}), including profiles created later",
            patch.display(),
            profiles.join(", ")
        )
    }
}

fn split_profiles(
    root: &Path,
    profiles: &[String],
    wired: fn(&Path, &str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut yes = Vec::new();
    let mut no = Vec::new();
    for name in profiles {
        if wired(root, name) {
            yes.push(name.clone());
        } else {
            no.push(name.clone());
        }
    }
    (yes, no)
}

fn profile_names(root: &Path) -> Vec<String> {
    let entries = match fs::read_dir(root.join("profiles")) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(%error, "dsh profiles");
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && path.join("package.json").is_file()
            && let Some(name) = entry.file_name().to_str().map(str::to_owned)
        {
            names.push(name);
        }
    }
    names.sort();
    names
}

fn legacy_capture(root: &Path, name: &str) -> bool {
    let dir = root.join("profiles").join(name);
    package_capture(&dir) || patch_mentions(&dir, true)
}

fn legacy_recall(root: &Path, name: &str) -> bool {
    patch_mentions(&root.join("profiles").join(name), false)
}

#[serde_as]
#[derive(Deserialize)]
struct ProfilePackage {
    #[serde(default)]
    #[serde_as(as = "BTreeMap<_, DefaultOnError>")]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    #[serde_as(as = "BTreeMap<_, DefaultOnError>")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dsh: Option<ProfileDsh>,
}

#[derive(Deserialize)]
struct ProfileDsh {
    #[serde(default)]
    profile: Option<ProfileBundles>,
}

#[derive(Deserialize)]
struct ProfileBundles {
    #[serde(default)]
    bundles: Vec<String>,
}

fn package_capture(dir: &Path) -> bool {
    let text = match fs::read_to_string(dir.join("package.json")) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "dsh profile package");
            return false;
        }
    };
    let package = match serde_json::from_str::<ProfilePackage>(&text) {
        Ok(package) => package,
        Err(error) => {
            tracing::debug!(%error, "dsh profile package");
            return false;
        }
    };
    let bundles = package
        .dsh
        .and_then(|section| section.profile)
        .map(|profile| profile.bundles)
        .unwrap_or_default();
    package
        .dependencies
        .values()
        .chain(package.dev_dependencies.values())
        .any(|spec| link_has_marker(dir, spec))
        || bundles
            .iter()
            .any(|name| contains_marker(&dir.join("node_modules").join(name)))
}

fn link_has_marker(dir: &Path, spec: &str) -> bool {
    let Some(raw) = spec
        .strip_prefix("link:")
        .or_else(|| spec.strip_prefix("file:"))
    else {
        return false;
    };
    let raw = raw.strip_prefix("file://").unwrap_or(raw);
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    contains_marker(&path)
}

fn patch_mentions(dir: &Path, capture: bool) -> bool {
    let text = match read_optional(&dir.join(PATCH_NAME)) {
        Ok(Some(text)) => text,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!(%error, "dsh profile patch");
            return false;
        }
    };
    let docs = match serde_yaml_ng::from_str::<Vec<Value>>(&text) {
        Ok(docs) => docs,
        Err(error) => {
            tracing::debug!(%error, "dsh profile patch");
            return false;
        }
    };
    rows_of(&docs).iter().any(|row| {
        if capture {
            resolved_plugin(dir, &row.name).is_some_and(|path| contains_marker(&path))
                || bare_module(dir, &row.name).is_some_and(|path| contains_marker(&path))
        } else {
            row_runs_rh(row)
        }
    })
}

fn bare_module(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.starts_with('.') || name.starts_with('/') || name.starts_with("file:") {
        return None;
    }
    Some(dir.join("node_modules").join(name))
}

fn resolved_plugin(base: &Path, name: &str) -> Option<PathBuf> {
    if let Some(path) = file_url(name) {
        return Some(path);
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if name.starts_with('.') {
        return Some(base.join(path));
    }
    None
}

fn file_url(name: &str) -> Option<PathBuf> {
    let rest = name.strip_prefix("file://")?;
    let path = rest.strip_prefix("//localhost").unwrap_or(rest);
    if path.starts_with("//") {
        return None;
    }
    Some(PathBuf::from(percent_decode(path)))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes.get(index) == Some(&b'%')
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Some(value) = hex_byte(hex)
        {
            out.push(value);
            index = index.saturating_add(3);
            continue;
        }
        if let Some(byte) = bytes.get(index) {
            out.push(*byte);
        }
        index = index.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_byte(bytes: &[u8]) -> Option<u8> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "dsh file url");
            return None;
        }
    };
    match u8::from_str_radix(text, 16) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::debug!(%error, "dsh file url");
            None
        }
    }
}

fn contains_marker(path: &Path) -> bool {
    let files = if path.is_dir() {
        vec![path.join("index.mjs"), path.join("index.js")]
    } else {
        vec![path.to_path_buf()]
    };
    files
        .iter()
        .any(|file| fs::read_to_string(file).is_ok_and(|text| text.contains(MARKER)))
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

fn read_patch(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.to_string()),
    }
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

fn validate_array(path: &Path, text: &str) -> Result<(), SetupError> {
    serde_yaml_ng::from_str::<Vec<Value>>(text)
        .map(|_| ())
        .map_err(|error| {
            config(
                path,
                format!("cordis.patch.yml would not be a YAML array: {error}"),
            )
        })
}

fn is_plain_empty_array(text: &str) -> bool {
    let mut arrays: usize = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[]" {
            arrays = arrays.saturating_add(1);
            continue;
        }
        return false;
    }
    arrays == 1 && serde_yaml_ng::from_str::<Vec<Value>>(text).is_ok_and(|items| items.is_empty())
}

fn replace_empty_array(text: &str, block: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        if !replaced && line.trim() == "[]" {
            out.push_str(block);
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

fn first_code(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

fn command_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        Some(_) | None => String::new(),
    }
}

fn arg_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(text)) => vec![text.clone()],
        Some(_) | None => Vec::new(),
    }
}

fn plain_or_quoted(text: &str) -> String {
    if is_plain(text) {
        text.to_owned()
    } else {
        yaml_single(text)
    }
}

fn is_plain(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && text != "true"
        && text != "false"
        && text != "null"
}

fn yaml_single(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn not_array(error: impl std::fmt::Display) -> String {
    format!("{NOT_ARRAY}: {error}")
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

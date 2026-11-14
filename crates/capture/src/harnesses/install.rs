use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::SetupError;
use serde::Serialize;

use super::codex_trust;
use super::entries::HookFile;
use super::entries::HookGroup;
use super::json_edit;
use super::locate::Locator;
use super::opencode_plugin;
use super::opencode_version::CHECKED;
use super::opencode_version::FIRST_LOADING;
use super::opencode_version::Fit;
use super::plan::FileEdit;
use super::plan::read_optional;
use super::registry::CaptureInstall;
use super::registry::CommandStyle;
use super::registry::HookEvent;
use super::registry::HookFileSpec;
use super::registry::Location;
use super::version::Version;

const OPENCODE_MARKER: &str = "capture hook --harness opencode";
const OPENCODE_PLUGIN: &str = "rivethub-capture.ts";
const OPENCODE_PLACEHOLDER: &str = "'__COMMAND__'";
const OPENCODE_TOO_OLD: &str = "the OpenCode on this machine is older than 1.3.4, which cannot load rh's capture plug-in; update OpenCode and connect again";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallView {
    pub(super) installed: bool,
    pub(super) connectable: bool,
    pub(super) detail: String,
}

pub(super) fn view(installed: bool, detail: String) -> InstallView {
    InstallView {
        installed,
        connectable: !installed,
        detail,
    }
}

fn io_error(path: &Path, source: io::Error) -> SetupError {
    SetupError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn config(path: &Path, message: impl Into<String>) -> SetupError {
    SetupError::Config {
        path: path.display().to_string(),
        message: message.into(),
    }
}

pub(super) fn detect(
    locator: &Locator,
    install: &CaptureInstall,
    id: &str,
    rh: &Path,
    version: Option<Version>,
) -> InstallView {
    match install {
        CaptureInstall::HookFile(hooks) if id == "codex" => codex_trust::overlay(
            detect_hooks(locator, hooks, id),
            codex_trust::read(locator, hooks, version),
        ),
        CaptureInstall::HookFile(hooks) => detect_hooks(locator, hooks, id),
        CaptureInstall::OpencodePlugin { directory } => {
            detect_opencode(locator, *directory, rh, Fit::of(version))
        }
        CaptureInstall::DshPlugin { .. } => {
            let status = super::dsh::capture_status(locator);
            view(status.installed, status.detail)
        }
    }
}

fn list_dir(directory: &Path) -> Result<Vec<PathBuf>, SetupError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(directory, error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(entry.map_err(|error| io_error(directory, error))?.path());
    }
    paths.sort();
    Ok(paths)
}

fn hook_files(target: &Path, style: CommandStyle) -> Result<Vec<PathBuf>, SetupError> {
    let mut files = vec![target.to_path_buf()];
    if style == CommandStyle::Argv
        && let Some(directory) = target.parent()
    {
        files.extend(
            list_dir(directory)?
                .into_iter()
                .filter(|path| path != target && path.extension().is_some_and(|ext| ext == "json")),
        );
    }
    Ok(files)
}

fn parse_hooks(path: &Path, text: &str) -> Result<HookFile, SetupError> {
    if text.trim().is_empty() {
        return Ok(HookFile::default());
    }
    serde_json::from_str(text).map_err(|error| config(path, error.to_string()))
}

fn satisfied(
    files: &[PathBuf],
    events: &[HookEvent],
    id: &str,
) -> Result<Vec<&'static str>, SetupError> {
    let mut found: Vec<&'static str> = Vec::new();
    for path in files {
        let Some(text) = read_optional(path)? else {
            continue;
        };
        let doc = parse_hooks(path, &text)?;
        for event in events {
            let present = doc
                .hooks
                .get(event.name)
                .is_some_and(|groups| groups.iter().any(|group| group.runs_capture(id)));
            if present && !found.contains(&event.name) {
                found.push(event.name);
            }
        }
    }
    Ok(found)
}

fn detect_hooks(locator: &Locator, hooks: &HookFileSpec, id: &str) -> InstallView {
    let HookFileSpec {
        file,
        events,
        style,
        ..
    } = *hooks;
    let Some(target) = locator.target(file) else {
        return view(false, "no home directory to look in".to_owned());
    };
    match hook_files(&target, style).and_then(|files| satisfied(&files, events, id)) {
        Err(error) => view(false, format!("cannot read hooks: {error}")),
        Ok(found) => {
            let missing: Vec<&str> = events
                .iter()
                .map(|event| event.name)
                .filter(|name| !found.contains(name))
                .collect();
            if missing.is_empty() {
                view(true, format!("hook installed in {}", target.display()))
            } else if missing.len() == events.len() {
                view(false, format!("no rh hook in {}", target.display()))
            } else {
                view(
                    false,
                    format!(
                        "hook in {} is missing {}",
                        target.display(),
                        missing.join(", ")
                    ),
                )
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginKind {
    Opencode,
}

fn plugin_files(directory: &Path, kind: PluginKind) -> Result<Vec<PathBuf>, SetupError> {
    let paths = list_dir(directory)?;
    Ok(match kind {
        PluginKind::Opencode => paths.into_iter().filter(|path| path.is_file()).collect(),
    })
}

fn marker(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::Opencode => OPENCODE_MARKER,
    }
}

fn plugin_with_marker(directory: &Path, kind: PluginKind) -> Result<Option<PathBuf>, SetupError> {
    for path in plugin_files(directory, kind)? {
        let text = fs::read_to_string(&path).map_err(|error| io_error(&path, error))?;
        if text.contains(marker(kind)) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn detect_opencode(locator: &Locator, directory: Location, rh: &Path, fit: Fit) -> InstallView {
    let Some(target) = locator.target(directory) else {
        return view(false, "no home directory to look in".to_owned());
    };
    if let Fit::TooOld(version) = fit {
        return InstallView {
            installed: false,
            connectable: false,
            detail: format!(
                "OpenCode {version} cannot load rh's capture plug-in; update OpenCode to {FIRST_LOADING} or later and run `rh harness connect opencode`"
            ),
        };
    }
    let found = match plugin_with_marker(&target, PluginKind::Opencode) {
        Err(error) => return view(false, format!("cannot read plug-ins: {error}")),
        Ok(None) => return view(false, format!("no rh plug-in in {}", target.display())),
        Ok(Some(path)) => path,
    };
    let ours = target.join(OPENCODE_PLUGIN);
    if found == ours {
        let current = match plugin_is_current(locator, rh, &ours) {
            Err(error) => return view(false, format!("cannot read plug-ins: {error}")),
            Ok(current) => current,
        };
        if !current {
            return view(
                false,
                format!(
                    "plug-in at {} is out of date and may not load; run `rh harness connect opencode` to replace it",
                    ours.display()
                ),
            );
        }
    }
    view(
        true,
        format!("plug-in installed at {}{}", found.display(), fit_note(fit)),
    )
}

fn plugin_is_current(locator: &Locator, rh: &Path, path: &Path) -> Result<bool, SetupError> {
    Ok(read_optional(path)? == Some(opencode_text(locator, rh)?))
}

fn fit_note(fit: Fit) -> String {
    match fit {
        Fit::Loads(version) => format!(" for OpenCode {version}"),
        Fit::Newer(version) => {
            format!(" for OpenCode {version}, newer than the versions rh has checked ({CHECKED})")
        }
        Fit::TooOld(_) | Fit::Unknown => String::new(),
    }
}

pub(super) fn connect_note(fit: Fit) -> String {
    match fit {
        Fit::Loads(version) => format!(
            "OpenCode {version} loads this plug-in when it starts; rh has checked capture on OpenCode {CHECKED}."
        ),
        Fit::Newer(version) => format!(
            "OpenCode {version} is newer than the versions rh has checked ({CHECKED}); after a session, `rh harness status` shows whether it captured."
        ),
        Fit::TooOld(_) | Fit::Unknown => format!(
            "rh could not read the OpenCode version; this plug-in loads on OpenCode {FIRST_LOADING} and later (checked on {CHECKED})."
        ),
    }
}

#[derive(Serialize)]
struct Group<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    matcher: Option<&'a str>,
    hooks: [Run<'a>; 1],
}

#[derive(Serialize)]
struct Run<'a> {
    r#type: &'static str,
    command: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<&'a str>,
    timeout: u32,
}

fn render_group(
    event: HookEvent,
    style: CommandStyle,
    rh: &str,
    id: &str,
    timeout: u32,
) -> Result<String, SetupError> {
    let line;
    let (command, args): (&str, Vec<&str>) = match style {
        CommandStyle::Argv => (rh, vec!["capture", "hook", "--harness", id]),
        CommandStyle::Line => {
            let quoted =
                shlex::try_quote(rh).map_err(|error| config(Path::new(rh), error.to_string()))?;
            line = if id == "claude-code" {
                format!("{quoted} capture hook")
            } else {
                format!("{quoted} capture hook --harness {id}")
            };
            (&line, Vec::new())
        }
    };
    serde_json::to_string_pretty(&Group {
        matcher: event.matcher,
        hooks: [Run {
            r#type: "command",
            command,
            args,
            timeout,
        }],
    })
    .map_err(|error| config(Path::new(rh), error.to_string()))
}

pub(super) fn plan_connect(
    locator: &Locator,
    id: &str,
    install: &CaptureInstall,
    rh: &Path,
    version: Option<Version>,
) -> Result<Vec<FileEdit>, SetupError> {
    match install {
        CaptureInstall::HookFile(hooks) => connect_hooks(locator, id, hooks, rh),
        CaptureInstall::OpencodePlugin { directory } => {
            connect_opencode(locator, *directory, rh, Fit::of(version))
        }
        CaptureInstall::DshPlugin { .. } => {
            let (edits, skipped) = super::dsh::plan(locator, rh, &[ConnectPart::Capture], true)?;
            if let Some(error) = skipped {
                return Err(error);
            }
            Ok(edits)
        }
    }
}

fn connect_hooks(
    locator: &Locator,
    id: &str,
    hooks: &HookFileSpec,
    rh: &Path,
) -> Result<Vec<FileEdit>, SetupError> {
    let HookFileSpec {
        file,
        events,
        style,
        timeout,
    } = *hooks;
    let target = locator
        .target(file)
        .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?;
    let found = satisfied(&hook_files(&target, style)?, events, id)?;
    let before = read_optional(&target)?;
    let rh = rh.to_string_lossy();
    let mut text = before.clone().unwrap_or_default();
    for event in events.iter().filter(|event| !found.contains(&event.name)) {
        let group = render_group(*event, style, &rh, id, timeout)?;
        text = json_edit::push_element(&text, &["hooks", event.name], &group)
            .map_err(|error| config(&target, error.to_string()))?;
    }
    if before.as_deref() == Some(text.as_str()) || (before.is_none() && text.is_empty()) {
        return Ok(Vec::new());
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(vec![FileEdit {
        path: target,
        before,
        after: Some(text),
    }])
}

fn opencode_template(locator: &Locator) -> Result<String, SetupError> {
    let Some(source) = locator.opencode_plugin() else {
        return Ok(opencode_plugin::TEMPLATE.to_owned());
    };
    let template = fs::read_to_string(source).map_err(|error| io_error(source, error))?;
    if template.contains(OPENCODE_PLACEHOLDER) {
        Ok(template)
    } else {
        Err(config(
            source,
            format!("the plug-in has no {OPENCODE_PLACEHOLDER} placeholder for rh's command"),
        ))
    }
}

fn opencode_text(locator: &Locator, rh: &Path) -> Result<String, SetupError> {
    let template = opencode_template(locator)?;
    let quoted = shlex::try_quote(&rh.to_string_lossy())
        .map_err(|error| config(rh, error.to_string()))?
        .into_owned();
    let command = format!("printf \"%s\" \"$1\" | exec {quoted} {OPENCODE_MARKER}");
    let literal = serde_json::to_string(&command).map_err(|error| config(rh, error.to_string()))?;
    Ok(template.replace(OPENCODE_PLACEHOLDER, &literal))
}

fn connect_opencode(
    locator: &Locator,
    directory: Location,
    rh: &Path,
    fit: Fit,
) -> Result<Vec<FileEdit>, SetupError> {
    if let Fit::TooOld(_) = fit {
        return Err(SetupError::Unsupported {
            id: "opencode".to_owned(),
            part: kernel::harness_setup::ConnectPart::Capture,
            reason: OPENCODE_TOO_OLD,
        });
    }
    let target = locator
        .target(directory)
        .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?;
    let path = target.join(OPENCODE_PLUGIN);
    let before = read_optional(&path)?;
    if before.is_none() && plugin_with_marker(&target, PluginKind::Opencode)?.is_some() {
        return Ok(Vec::new());
    }
    let after = opencode_text(locator, rh)?;
    if before.as_deref() == Some(after.as_str()) {
        return Ok(Vec::new());
    }
    Ok(vec![FileEdit {
        before,
        after: Some(after),
        path,
    }])
}

pub(super) fn plan_disconnect(
    locator: &Locator,
    id: &str,
    install: &CaptureInstall,
) -> Result<Vec<FileEdit>, SetupError> {
    match install {
        CaptureInstall::HookFile(hooks) => disconnect_hooks(locator, id, hooks.file, hooks.style),
        CaptureInstall::OpencodePlugin { directory } => {
            let Some(target) = locator.target(*directory) else {
                return Ok(Vec::new());
            };
            let path = target.join(OPENCODE_PLUGIN);
            let Some(before) = read_optional(&path)? else {
                return Ok(Vec::new());
            };
            if !before.contains(OPENCODE_MARKER) {
                return Ok(Vec::new());
            }
            Ok(vec![FileEdit {
                path,
                before: Some(before),
                after: None,
            }])
        }
        CaptureInstall::DshPlugin { .. } => {
            super::dsh::plan(locator, Path::new(""), &[ConnectPart::Capture], false)
                .map(|(edits, _)| edits)
        }
    }
}

fn disconnect_hooks(
    locator: &Locator,
    id: &str,
    file: Location,
    style: CommandStyle,
) -> Result<Vec<FileEdit>, SetupError> {
    let Some(target) = locator.target(file) else {
        return Ok(Vec::new());
    };
    let mut edits = Vec::new();
    for path in hook_files(&target, style)? {
        let Some(before) = read_optional(&path)? else {
            continue;
        };
        let doc = parse_hooks(&path, &before)?;
        let mut text = before.clone();
        for (event, groups) in &doc.hooks {
            if !groups.iter().any(|group| group.is_rh_capture(id)) {
                continue;
            }
            text = json_edit::remove_elements(&text, &["hooks", event], |raw| {
                serde_json::from_str::<HookGroup>(raw).is_ok_and(|group| group.is_rh_capture(id))
            })
            .and_then(|text| json_edit::prune_empty(&text, &["hooks", event]))
            .map_err(|error| config(&path, error.to_string()))?;
        }
        if text == before {
            continue;
        }
        let remaining = parse_hooks(&path, &text)?;
        let dedicated = style == CommandStyle::Argv && path == target;
        let after = if dedicated && remaining.hooks.is_empty() {
            None
        } else {
            Some(text)
        };
        edits.push(FileEdit {
            path,
            before: Some(before),
            after,
        });
    }
    Ok(edits)
}

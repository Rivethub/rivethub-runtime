use std::collections::BTreeMap;
use std::path::Path;

use kernel::harness_setup::SetupError;
use serde::Deserialize;
use serde::Serialize;

use super::entries::HookCommand;
use super::entries::capture_hook_harness;
use super::install::InstallView;
use super::install::config;
use super::install::view;
use super::json_edit;
use super::locate::Locator;
use super::plan::FileEdit;
use super::plan::read_optional;
use super::registry::Location;

const ANTIGRAVITY_HOOK_NAME: &str = "rivethub-capture";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AntigravityEvent {
    PostInvocation,
    Stop,
}

impl AntigravityEvent {
    const ALL: [Self; 2] = [Self::PostInvocation, Self::Stop];

    const fn name(self) -> &'static str {
        match self {
            Self::PostInvocation => "PostInvocation",
            Self::Stop => "Stop",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AntigravityNamedHook {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(rename = "PostInvocation", default)]
    post_invocation: Vec<HookCommand>,
    #[serde(rename = "Stop", default)]
    stop: Vec<HookCommand>,
}

impl AntigravityNamedHook {
    fn active(&self) -> bool {
        self.enabled != Some(false)
    }

    fn events(&self, id: &str) -> Vec<AntigravityEvent> {
        let mut found = Vec::new();
        if self.active() {
            if self
                .post_invocation
                .iter()
                .any(|cmd| capture_hook_harness(&cmd.invocation()).is_some_and(|named| named == id))
            {
                found.push(AntigravityEvent::PostInvocation);
            }
            if self
                .stop
                .iter()
                .any(|cmd| capture_hook_harness(&cmd.invocation()).is_some_and(|named| named == id))
            {
                found.push(AntigravityEvent::Stop);
            }
        }
        found
    }

    fn is_rh_capture(&self, id: &str) -> bool {
        let all: Vec<&HookCommand> = self
            .post_invocation
            .iter()
            .chain(self.stop.iter())
            .collect();
        !all.is_empty()
            && all
                .into_iter()
                .all(|cmd| capture_hook_harness(&cmd.invocation()).is_some_and(|named| named == id))
    }
}

#[derive(Serialize)]
struct AntigravityRun<'a> {
    r#type: &'static str,
    command: &'a str,
    timeout: u32,
}

#[derive(Serialize)]
struct AntigravityHookEntry<'a> {
    #[serde(rename = "PostInvocation")]
    post_invocation: [AntigravityRun<'a>; 1],
    #[serde(rename = "Stop")]
    stop: [AntigravityRun<'a>; 1],
}

fn parse(path: &Path, text: &str) -> Result<BTreeMap<String, AntigravityNamedHook>, SetupError> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(text).map_err(|error| config(path, error.to_string()))
}

pub(super) fn detect(locator: &Locator, file: Location, id: &str) -> InstallView {
    let Some(target) = locator.target(file) else {
        return view(false, "no home directory to look in".to_owned());
    };
    let text = match read_optional(&target) {
        Ok(Some(text)) => text,
        Ok(None) => return view(false, format!("no rh hook in {}", target.display())),
        Err(error) => return view(false, format!("cannot read hooks: {error}")),
    };
    let doc = match parse(&target, &text) {
        Ok(doc) => doc,
        Err(error) => return view(false, format!("cannot read hooks: {error}")),
    };
    let mut found = Vec::new();
    for entry in doc.values() {
        for event in entry.events(id) {
            if !found.contains(&event) {
                found.push(event);
            }
        }
    }
    let missing: Vec<&str> = AntigravityEvent::ALL
        .into_iter()
        .filter(|event| !found.contains(event))
        .map(AntigravityEvent::name)
        .collect();
    if missing.is_empty() {
        view(true, format!("hook installed in {}", target.display()))
    } else if missing.len() == AntigravityEvent::ALL.len() {
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

pub(super) fn connect(
    locator: &Locator,
    id: &str,
    file: Location,
    timeout: u32,
    rh: &Path,
) -> Result<Vec<FileEdit>, SetupError> {
    let target = locator
        .target(file)
        .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?;
    let before = read_optional(&target)?;
    let existing = match &before {
        Some(text) => parse(&target, text)?,
        None => BTreeMap::new(),
    };
    let mut found = Vec::new();
    for entry in existing.values() {
        for event in entry.events(id) {
            if !found.contains(&event) {
                found.push(event);
            }
        }
    }
    if AntigravityEvent::ALL
        .iter()
        .all(|event| found.contains(event))
    {
        return Ok(Vec::new());
    }
    let rh_text = rh.to_string_lossy();
    let quoted = shlex::try_quote(&rh_text).map_err(|error| config(rh, error.to_string()))?;
    let command = format!("{quoted} capture hook --harness {id}");
    let entry = serde_json::to_string_pretty(&AntigravityHookEntry {
        post_invocation: [AntigravityRun {
            r#type: "command",
            command: &command,
            timeout,
        }],
        stop: [AntigravityRun {
            r#type: "command",
            command: &command,
            timeout,
        }],
    })
    .map_err(|error| config(rh, error.to_string()))?;
    let current = before.clone().unwrap_or_default();
    let mut text = json_edit::insert_member(&current, &[], ANTIGRAVITY_HOOK_NAME, &entry)
        .map_err(|error| config(&target, error.to_string()))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(vec![FileEdit {
        path: target,
        before,
        after: Some(text),
    }])
}

pub(super) fn disconnect(
    locator: &Locator,
    id: &str,
    file: Location,
) -> Result<Vec<FileEdit>, SetupError> {
    let Some(target) = locator.target(file) else {
        return Ok(Vec::new());
    };
    let Some(before) = read_optional(&target)? else {
        return Ok(Vec::new());
    };
    let doc = parse(&target, &before)?;
    let ours: Vec<String> = doc
        .iter()
        .filter(|(name, entry)| *name == ANTIGRAVITY_HOOK_NAME || entry.is_rh_capture(id))
        .map(|(name, _)| name.clone())
        .collect();
    if ours.is_empty() {
        return Ok(Vec::new());
    }
    let text = json_edit::remove_members(&before, &[], |name| ours.iter().any(|ours| ours == name))
        .map_err(|error| config(&target, error.to_string()))?;
    if text == before {
        return Ok(Vec::new());
    }
    Ok(vec![FileEdit {
        path: target,
        before: Some(before),
        after: Some(text),
    }])
}

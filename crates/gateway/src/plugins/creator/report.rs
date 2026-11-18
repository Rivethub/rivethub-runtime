use std::fmt::{self, Write as _};
use std::path::Path;

use wire::plugin::PluginId;
use wire::plugin::host::{PluginList, PluginLogLine, PluginRecord, PluginState};
use wire::plugin::manifest::Permission;

use super::super::HostError;
use super::super::supervisor::describe;
use super::PageCheck;
use super::scaffold::{GUIDE_FILE, Made, Template};

pub(super) fn templates(
    templates: &[Template],
    root: &Path,
    node: &str,
) -> Result<String, fmt::Error> {
    let mut text = String::new();
    if templates.is_empty() {
        write!(
            text,
            "Node {node} holds no plug-in templates: {} is empty or missing. A person installs them from the rivethub-plugins repository (npm run templates:install there writes them into that folder). You cannot start a plug-in until then: tell them.",
            root.display()
        )?;
        return Ok(text);
    }
    writeln!(
        text,
        "Templates on node {node}, in {}, each a working plug-in to start from:",
        root.display()
    )?;
    for template in templates {
        match &template.found {
            Ok(manifest) => writeln!(
                text,
                "- {}: {} {}. {} Runs: {}. Files: {}.",
                template.name,
                manifest.name,
                manifest.version,
                manifest.description,
                manifest.run.command.join(" "),
                template.files.join(", ")
            )?,
            Err(problem) => writeln!(text, "- {}: not usable: {problem}", template.name)?,
        }
    }
    write!(text, "Call plugins-scaffold with one of these names.")?;
    Ok(text)
}

pub(super) fn scaffolded(made: &Made, id: &PluginId) -> Result<String, fmt::Error> {
    let folder = made.folder.display();
    let mut text = String::new();
    writeln!(
        text,
        "Created {folder} from the template {}, with the id {}. Files: {}.",
        made.template,
        id.as_ref(),
        made.files.join(", ")
    )?;
    if made.files.iter().any(|file| file == GUIDE_FILE) {
        write!(text, "Next: read {folder}/{GUIDE_FILE}, rewrite")?;
    } else {
        write!(text, "Next: rewrite")?;
    }
    write!(
        text,
        " the example into what was asked for, then call plugins-install with the id {}.",
        id.as_ref()
    )?;
    Ok(text)
}

pub(super) fn not_valid(error: &HostError) -> String {
    if let HostError::Invalid(detail) = error {
        format!("The manifest is not valid, so nothing changed: {detail}")
    } else {
        error.to_string()
    }
}

pub(super) fn installed(
    verb: &str,
    record: &PluginRecord,
    pages: &[PageCheck],
    lines: &[PluginLogLine],
) -> Result<String, fmt::Error> {
    let mut text = String::new();
    writeln!(
        text,
        "{verb} {} ({} {}) from {}.\n{}",
        record.id.as_ref(),
        record.manifest.name,
        record.manifest.version,
        record.path,
        standing(record)
    )?;
    for page in pages {
        let view = page.view.as_ref();
        match &page.outcome {
            Ok(()) => writeln!(
                text,
                "Page {} (view {view}): rh accepts the view it answers.",
                page.title
            )?,
            Err(detail) => writeln!(
                text,
                "Page {} (view {view}): {detail}. The app shows an error there instead; fix it and call plugins-install again.",
                page.title
            )?,
        }
    }
    exits(&mut text, record)?;
    log(&mut text, lines)?;
    Ok(text)
}

pub(super) fn status(record: &PluginRecord, lines: &[PluginLogLine]) -> Result<String, fmt::Error> {
    let mut text = String::new();
    writeln!(
        text,
        "{} ({} {}) on node {}, from {}.\n{}\nPermissions granted: {}. Waiting for a person: {}.",
        record.id.as_ref(),
        record.manifest.name,
        record.manifest.version,
        record.node,
        record.path,
        standing(record),
        names(&record.granted),
        names(&record.pending)
    )?;
    exits(&mut text, record)?;
    if lines.is_empty() {
        writeln!(text, "Its log is empty.")?;
    } else {
        log(&mut text, lines)?;
    }
    Ok(text)
}

pub(super) fn listed(list: &PluginList, workshop: &Path) -> Result<String, fmt::Error> {
    let mut text = String::new();
    if list.plugins.is_empty() {
        write!(
            text,
            "No plug-in is installed on node {}. Start one with plugins-scaffold; it goes in the workshop {}.",
            list.node,
            workshop.display()
        )?;
        return Ok(text);
    }
    writeln!(
        text,
        "Plug-ins on node {} (the workshop is {}):",
        list.node,
        workshop.display()
    )?;
    for record in &list.plugins {
        writeln!(
            text,
            "- {} ({}): {}, from {}",
            record.id.as_ref(),
            record.manifest.name,
            state_words(record),
            record.path
        )?;
    }
    write!(
        text,
        "Call plugins-status with an id to read that plug-in's log."
    )?;
    Ok(text)
}

fn standing(record: &PluginRecord) -> String {
    let name = &record.manifest.name;
    let place = format!(
        "in the RivetHub app under Settings › Plug-ins › {name}, on a device paired with node {}",
        record.node
    );
    let error = record.error.as_deref().unwrap_or("it stopped");
    match record.state {
        PluginState::Running => "It is running.".to_owned(),
        PluginState::Starting => {
            "It is still starting; call plugins-status in a moment to see how it went.".to_owned()
        }
        PluginState::Disabled if record.pending.is_empty() => format!(
            "It is off. A person has to turn it on {place} (Turn on). You cannot do that: tell them, and call plugins-status once they have."
        ),
        PluginState::Disabled => format!(
            "It is off and asks for {}. A person has to approve that and turn it on {place} (Approve and turn on). You cannot approve permissions: tell them, and call plugins-status once they have.",
            names(&record.pending)
        ),
        PluginState::AwaitingApproval => format!(
            "It is stopped: the manifest now asks for {}, which a person has to approve {place} (Approve and turn on). Tell them, or take the new permissions out of the manifest and call plugins-install again.",
            names(&record.pending)
        ),
        PluginState::Backoff => format!(
            "It is not running: {error}. Fix what the log shows and call plugins-install again."
        ),
        PluginState::Invalid => format!(
            "Its manifest is no longer valid: {error}. Fix that field and call plugins-install again."
        ),
    }
}

fn state_words(record: &PluginRecord) -> String {
    match record.state {
        PluginState::Running => "running".to_owned(),
        PluginState::Starting => "starting".to_owned(),
        PluginState::Disabled if record.pending.is_empty() => "off".to_owned(),
        PluginState::Disabled => format!("off, asking for {}", names(&record.pending)),
        PluginState::AwaitingApproval => {
            format!("stopped until a person approves {}", names(&record.pending))
        }
        PluginState::Backoff => "failing and restarting".to_owned(),
        PluginState::Invalid => "manifest invalid".to_owned(),
    }
}

fn exits(text: &mut String, record: &PluginRecord) -> fmt::Result {
    match &record.last_exit {
        Some(exit) => writeln!(
            text,
            "Last exit: {} at {} UTC; {} restarts since rh started.",
            describe(exit),
            exit.at.strftime("%H:%M:%S"),
            record.restarts
        ),
        None => Ok(()),
    }
}

fn log(text: &mut String, lines: &[PluginLogLine]) -> fmt::Result {
    if lines.is_empty() {
        return Ok(());
    }
    writeln!(text, "Its last {} log lines, oldest first:", lines.len())?;
    for line in lines {
        writeln!(
            text,
            "{} {} {}",
            line.at.strftime("%H:%M:%S"),
            line.stream,
            line.text
        )?;
    }
    Ok(())
}

fn names(permissions: &[Permission]) -> String {
    if permissions.is_empty() {
        return "none".to_owned();
    }
    permissions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

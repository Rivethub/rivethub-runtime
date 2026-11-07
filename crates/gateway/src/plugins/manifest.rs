use std::collections::BTreeSet;

use wire::plugin::manifest::PluginManifest;
use wire::plugin::view::ViewInput;

const RESERVED_ENV_PREFIX: &str = "RIVETHUB_";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub(super) struct ManifestProblem {
    pub(super) path: String,
    pub(super) message: String,
}

fn problem(path: String, message: &str) -> ManifestProblem {
    ManifestProblem {
        path,
        message: message.to_owned(),
    }
}

fn first_repeat<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut seen = BTreeSet::new();
    names.into_iter().find(|name| !seen.insert(*name))
}

fn check_program(program: Option<&String>) -> Result<(), ManifestProblem> {
    let Some(program) = program.filter(|first| !first.is_empty()) else {
        return Err(problem("run.command".into(), "is empty"));
    };
    let path = std::path::Path::new(program);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(problem(
            "run.command[0]".into(),
            "must be a program on PATH or a path inside the plug-in's folder",
        ));
    }
    Ok(())
}

pub(super) fn check_manifest(manifest: &PluginManifest) -> Result<(), ManifestProblem> {
    if manifest.id.as_ref() == super::creator::NAMESPACE {
        return Err(problem(
            "id".into(),
            "plugins is reserved for rh's own plug-in tools",
        ));
    }
    check_program(manifest.run.command.first())?;
    if let Some(name) = manifest
        .run
        .env
        .keys()
        .find(|name| name.starts_with(RESERVED_ENV_PREFIX))
    {
        return Err(problem(
            format!("run.env.{name}"),
            "RIVETHUB_ names are reserved",
        ));
    }
    for (index, field) in manifest.settings.iter().enumerate() {
        if matches!(field.input, ViewInput::File { .. }) {
            return Err(problem(
                format!("settings[{index}].input"),
                "a setting cannot be a file",
            ));
        }
    }
    let contributes = &manifest.contributes;
    let repeats = [
        (
            "settings",
            first_repeat(manifest.settings.iter().map(|field| field.key.as_str())),
        ),
        (
            "tools",
            first_repeat(manifest.tools.iter().map(|tool| tool.name.as_ref())),
        ),
        (
            "jobs",
            first_repeat(manifest.jobs.iter().map(|job| job.id.as_ref())),
        ),
        (
            "contributes",
            first_repeat(
                contributes
                    .pages
                    .iter()
                    .map(|page| page.id.as_ref())
                    .chain(contributes.topic_cards.iter().map(|card| card.id.as_ref()))
                    .chain(
                        contributes
                            .session_actions
                            .iter()
                            .map(|action| action.id.as_ref()),
                    ),
            ),
        ),
    ];
    for (path, repeat) in repeats {
        if let Some(name) = repeat {
            return Err(problem(path.into(), &format!("{name} is declared twice")));
        }
    }
    for (index, kind) in manifest.events.iter().enumerate() {
        let needed = super::rules::event_permission(*kind);
        if !manifest.permissions.contains(&needed) {
            return Err(problem(
                format!("events[{index}]"),
                &format!("needs the {needed} permission"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wire::plugin::manifest::PluginManifest;

    use super::check_manifest;

    const RUN: &str = r#""run": {"command": ["files"]}"#;

    fn manifest(body: &str) -> Result<PluginManifest, serde_json::Error> {
        let text = format!(
            r#"{{"manifest": 1, "id": "files", "name": "Files", "version": "0.1.0",
            "description": "Files.", "permissions": ["sessions.read"], {body}}}"#
        );
        serde_json::from_str(&text)
    }

    #[test]
    fn manifest_problems_name_the_field() {
        assert!(check_manifest(&manifest(RUN).unwrap()).is_ok());
        let problems = [
            (
                format!(r#"{RUN}, "events": ["summary.written"]"#),
                "events[0]",
            ),
            (r#""run": {"command": []}"#.to_owned(), "run.command"),
            (
                r#""run": {"command": ["/usr/bin/node"]}"#.to_owned(),
                "run.command[0]",
            ),
            (
                r#""run": {"command": ["bin/../../.cargo/bin/cargo"]}"#.to_owned(),
                "run.command[0]",
            ),
            (
                r#""run": {"command": ["x"], "env": {"RIVETHUB_PLUGIN_TOKEN": "x"}}"#.to_owned(),
                "run.env.RIVETHUB_PLUGIN_TOKEN",
            ),
            (
                format!(
                    r#"{RUN}, "settings": [{{"key": "a", "label": "A", "input": {{"type": "file"}}}}]"#
                ),
                "settings[0].input",
            ),
            (
                format!(
                    r#"{RUN}, "jobs": [{{"id": "a", "title": "A"}}, {{"id": "a", "title": "B"}}]"#
                ),
                "jobs",
            ),
        ];
        for (body, path) in problems {
            assert_eq!(
                check_manifest(&manifest(&body).unwrap()).unwrap_err().path,
                path,
                "{body}"
            );
        }
    }
}

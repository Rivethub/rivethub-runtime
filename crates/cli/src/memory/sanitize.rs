use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use config::Environment;
use config::MappingFile;

use super::bundle::write_whole;
use super::parse::MemoryCommand;
use super::parse::nonempty;
use super::parse::reject;
use crate::Error;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;

pub(super) fn args(args: &[String]) -> Result<MemoryCommand, Error> {
    let mut scan = Scan::new(args);
    let mut input = None;
    let mut out = None;
    let mut mapping = None;
    while let Some(token) = scan.current().map(str::to_owned) {
        match scan.option("--out", None) {
            Taken::Value(value) => {
                out = Some(nonempty(&value, Error::OutRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::OutRequired),
            Taken::Absent => {}
        }
        match scan.option("--mapping", None) {
            Taken::Value(value) => {
                mapping = Some(nonempty(&value, Error::MappingRequired)?);
                continue;
            }
            Taken::Missing => return Err(Error::MappingRequired),
            Taken::Absent => {}
        }
        if token.starts_with('-') {
            return Err(reject(&scan));
        }
        if input.is_some() {
            return Err(Error::ImportOne);
        }
        input = Some(PathBuf::from(token));
        scan.bump();
    }
    let Some(input) = input else {
        return Err(Error::ImportPath);
    };
    let Some(out) = out else {
        return Err(Error::OutRequired);
    };
    Ok(MemoryCommand::Sanitize {
        input,
        out,
        mapping,
    })
}

pub(super) async fn run(
    input: &Path,
    out: &Path,
    mapping: Option<PathBuf>,
    env: &Environment,
) -> Result<Report, Error> {
    if tokio::fs::metadata(out)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Error::BundleOutDirectory(out.display().to_string()));
    }
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(input).await?)?;
    let mapping = match mapping {
        Some(path) => MappingFile::Configured(path),
        None => super::target::load(env)?
            .unwrap_or_default()
            .sanitize_mapping(env)?,
    };
    let (mapping, source) = rules(&mapping).await?;
    let key = capture::sanitize::LocalKey::load_or_create(&key_path(env)?)?;
    let policy = capture::sanitize::Policy { mapping, key };
    let sanitized = capture::sanitize::run(&bundle, &policy, Utc::now())?;
    let report_path = report_path(out);
    let report = sanitized
        .report
        .markdown(&out.display().to_string(), &source);
    write_whole(report_path.clone(), report.into_bytes()).await?;
    let mut text = sanitized.sealed.text;
    text.push(b'\n');
    write_whole(out.to_path_buf(), text).await?;
    let totals = sanitized.report.totals();
    let count = bundle.sessions.len();
    let noun = if count == 1 { "session" } else { "sessions" };
    let mut out = format!(
        "sanitized {count} {noun}: {} secrets, {} mapped, {} redacted; report at {}\n",
        totals.secrets,
        totals.mapped,
        totals.redacted,
        report_path.display()
    );
    out.push_str(&summary(&sanitized.report));
    Ok(Report::success(out))
}

fn summary(report: &capture::sanitize::Report) -> String {
    let mut by_rule: Vec<(String, usize)> = Vec::new();
    for change in &report.changes {
        let name = change.rule.to_string();
        match by_rule.iter_mut().find(|(rule, _)| *rule == name) {
            Some((_, count)) => *count += 1,
            None => by_rule.push((name, 1)),
        }
    }
    by_rule.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let rules = if by_rule.is_empty() {
        "no rule matched".to_owned()
    } else {
        by_rule
            .iter()
            .map(|(rule, count)| format!("{rule} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "review summary: changes by rule: {rules}\n\
         not scanned: dates, roles, finalize; numbers, booleans and null stay typed\n\
         the sanitizer is best-effort: it catches known vendor tokens, assignments, URL credentials, PEM blocks and mapped terms, and can miss unusual code forms and new vendor shapes; read the report before you share the bundle\n"
    )
}

async fn rules(
    mapping: &MappingFile,
) -> Result<(capture::sanitize::mapping::Mapping, String), Error> {
    let (path, required) = match mapping {
        MappingFile::Configured(path) => (path, true),
        MappingFile::Default(path) => (path, false),
    };
    let display = path.display().to_string();
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                capture::sanitize::mapping::Mapping::default(),
                format!("none ({display} does not exist)"),
            ));
        }
        Err(source) => {
            return Err(Error::MappingRead {
                path: display,
                source,
            });
        }
    };
    let rules = capture::sanitize::mapping::parse(&text).map_err(|source| Error::Mapping {
        path: display.clone(),
        source,
    })?;
    let count = rules.rules().len();
    let noun = if count == 1 { "rule" } else { "rules" };
    Ok((rules, format!("{display} ({count} {noun})")))
}

fn key_path(env: &Environment) -> Result<PathBuf, Error> {
    let state = match env.text("XDG_STATE_HOME").filter(|text| !text.is_empty()) {
        Some(text) => PathBuf::from(text),
        None => config::Paths::new(env)?.home().join(".local").join("state"),
    };
    Ok(capture::sanitize::LocalKey::path_in(&state))
}

fn report_path(out: &Path) -> PathBuf {
    let base = if out.extension().is_some_and(|extension| extension == "json") {
        out.with_extension("")
    } else {
        out.to_path_buf()
    };
    let mut name = base.into_os_string();
    name.push(".report.md");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn sanitize_takes_a_bundle_an_out_file_and_an_optional_mapping() {
        let parsed = args(&strings(&[
            "s.json",
            "--out",
            "s.clean.json",
            "--mapping",
            "m.toml",
        ]));
        assert!(matches!(
            parsed,
            Ok(MemoryCommand::Sanitize { input, out, mapping: Some(mapping) })
                if input == Path::new("s.json") && out == Path::new("s.clean.json") && mapping == Path::new("m.toml")
        ));
        assert!(matches!(
            args(&strings(&["s.json"])),
            Err(Error::OutRequired)
        ));
        assert!(matches!(
            args(&strings(&["--out", "s.clean.json"])),
            Err(Error::ImportPath)
        ));
    }

    #[test]
    fn the_report_sits_beside_the_bundle() {
        assert_eq!(
            report_path(Path::new("out/s.clean.json")),
            PathBuf::from("out/s.clean.report.md")
        );
        assert_eq!(
            report_path(Path::new("bundle")),
            PathBuf::from("bundle.report.md")
        );
    }
}

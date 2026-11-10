use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;

use capture::sanitize::MarkState;
use capture::sanitize::decide::Endpoint;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Resolution;
use capture::sanitize::decide::Review;
use chrono::Utc;
use config::Config;
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
    let mut report = None;
    let mut decide = false;
    let mut block = false;
    let mut learn = false;
    while let Some(token) = scan.current().map(str::to_owned) {
        if let Some(set) = [
            ("--decide", &mut decide),
            ("--block", &mut block),
            ("--learn", &mut learn),
        ]
        .into_iter()
        .find_map(|(name, set)| scan.flag(name).then_some(set))
        {
            *set = true;
            continue;
        }
        if path_option(&mut scan, "--out", || Error::OutRequired, &mut out)?
            || path_option(
                &mut scan,
                "--mapping",
                || Error::MappingRequired,
                &mut mapping,
            )?
            || path_option(
                &mut scan,
                "--resolve",
                || Error::ResolveRequired,
                &mut report,
            )?
        {
            continue;
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
    if let Some(report) = report {
        if out.is_some() || decide || block {
            return Err(Error::ResolveAlone);
        }
        return Ok(MemoryCommand::Resolve {
            report,
            bundle: input,
            mapping,
            learn,
        });
    }
    if learn {
        return Err(Error::LearnWithoutResolve);
    }
    if block && !decide {
        return Err(Error::BlockWithoutDecide);
    }
    let Some(input) = input else {
        return Err(Error::ImportPath);
    };
    let Some(out) = out else {
        return Err(Error::OutRequired);
    };
    Ok(MemoryCommand::Sanitize(Job {
        input,
        out,
        mapping,
        decide,
        block,
    }))
}

fn path_option(
    scan: &mut Scan<'_>,
    name: &str,
    missing: fn() -> Error,
    slot: &mut Option<PathBuf>,
) -> Result<bool, Error> {
    match scan.option(name, None) {
        Taken::Value(value) => {
            *slot = Some(nonempty(&value, missing())?);
            Ok(true)
        }
        Taken::Missing => Err(missing()),
        Taken::Absent => Ok(false),
    }
}

#[derive(Debug)]
pub(super) struct Job {
    input: PathBuf,
    out: PathBuf,
    mapping: Option<PathBuf>,
    decide: bool,
    block: bool,
}

pub(super) async fn run(job: Job, env: &Environment) -> Result<Report, Error> {
    let Job {
        input,
        out,
        mapping,
        decide,
        block,
    } = job;
    let (input, out) = (input.as_path(), out.as_path());
    if tokio::fs::metadata(out)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Error::BundleOutDirectory(out.display().to_string()));
    }
    let endpoint = if decide {
        Some(endpoint(
            &super::target::load(env)?.unwrap_or_default(),
            block,
        )?)
    } else {
        None
    };
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(input).await?)?;
    let mapping = mapping_file(mapping, env)?;
    let (mapping, source) = rules(&mapping).await?;
    let key = capture::sanitize::LocalKey::load_or_create(&key_path(env)?)?;
    let policy = capture::sanitize::Policy { mapping, key };
    let mut scrubbed = capture::sanitize::scrub(&bundle, &policy)?;
    let report_path = report_path(out);
    let decided = if let Some((endpoint, review)) = endpoint {
        let candidates = scrubbed.candidates();
        let scores = capture::sanitize::decide::score(&endpoint, &candidates.chunks).await?;
        let decision = scrubbed.flag(candidates, &scores, review)?;
        decision_line(decision, &report_path)
    } else {
        String::new()
    };
    let sanitized = scrubbed.seal(&policy, Utc::now())?;
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
        "sanitized {count} {noun}: {totals}; report at {}\n{decided}",
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
         the sanitizer is best-effort: it catches known vendor tokens, assignments, name/value pairs, URL credentials, PEM blocks and mapped terms, and can miss unusual code forms, new vendor shapes and bare-word passwords in prose; read the report before you share the bundle\n"
    )
}

fn endpoint(config: &Config, block: bool) -> Result<(Endpoint, Review), Error> {
    let section = config
        .sanitize_decide()
        .filter(|section| !section.url.trim().is_empty())
        .ok_or(Error::DecideUnconfigured)?;
    if !(section.threshold > 0.0 && section.threshold <= 1.0) {
        return Err(Error::DecideThreshold(section.threshold));
    }
    let concurrency = NonZeroUsize::new(section.concurrency)
        .filter(|concurrency| concurrency.get() <= MAX_CONCURRENCY)
        .ok_or(Error::DecideConcurrency(section.concurrency))?;
    let endpoint = Endpoint {
        url: section.url.trim().to_owned(),
        concurrency,
    };
    let review = Review {
        top: NonZeroUsize::new(section.top).ok_or(Error::DecideTop)?,
        threshold: section.threshold,
        mode: if block || section.block {
            Mode::Block
        } else {
            Mode::Advisory
        },
    };
    Ok((endpoint, review))
}

const MAX_CONCURRENCY: usize = 16;

fn decision_line(decision: &capture::sanitize::Decision, report: &Path) -> String {
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let flagged = decision.flags.len();
    let review = decision.review;
    let next = match (flagged, review.mode) {
        (0, _) => String::new(),
        (_, Mode::Advisory) => format!(
            "; review them in {} (advisory: ingest does not wait)",
            report.display()
        ),
        (_, Mode::Block) => format!(
            "; mark them in {} (block: the bundle withholds them until you do)",
            report.display()
        ),
    };
    format!(
        "decision pass: {} scored {} in {}, {} skipped; {} for review (top {} per session at or over {:.2}){next}\n",
        decision.model,
        count(decision.scored, "chunk", "chunks"),
        count(decision.calls, "call", "calls"),
        count(decision.trivial, "trivial chunk", "trivial chunks"),
        count(flagged, "flag", "flags"),
        review.top,
        review.threshold,
    )
}

fn mapping_file(mapping: Option<PathBuf>, env: &Environment) -> Result<MappingFile, Error> {
    Ok(match mapping {
        Some(path) => MappingFile::Configured(path),
        None => super::target::load(env)?
            .unwrap_or_default()
            .sanitize_mapping(env)?,
    })
}

pub(super) async fn resolve(
    report: &Path,
    bundle: Option<PathBuf>,
    mapping: Option<PathBuf>,
    learn: bool,
    env: &Environment,
) -> Result<Report, Error> {
    let text = tokio::fs::read_to_string(report).await?;
    let flags = capture::sanitize::marks::read_flags(&text).map_err(|source| Error::Report {
        path: report.display().to_string(),
        source,
    })?;
    let bundle_path = match bundle {
        Some(path) => path,
        None => bundle_beside(report).await?,
    };
    if learn {
        for flag in &flags.flags {
            flag.learnable().map_err(|source| Error::Report {
                path: report.display().to_string(),
                source,
            })?;
        }
    }
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(&bundle_path).await?)?;
    let key = capture::sanitize::LocalKey::load_or_create(&key_path(env)?)?;
    let resolved = capture::sanitize::resolve(&bundle, &flags, &key)?;
    if resolved.sealed.bundle != bundle {
        let mut sealed = resolved.sealed.text.clone();
        sealed.push(b'\n');
        write_whole(bundle_path.clone(), sealed).await?;
    }
    let applied: Vec<usize> = resolved.applied.iter().map(|(number, _)| *number).collect();
    let rewritten = flags.rewrite(&text, resolved.open, resolved.total, &applied);
    if rewritten != text {
        write_whole(report.to_path_buf(), rewritten.into_bytes()).await?;
    }
    let mut maps: Vec<(&str, &str)> = Vec::new();
    for flag in &flags.flags {
        if let MarkState::Marked(Resolution::Map { term, placeholder })
        | MarkState::Resolved(Resolution::Map { term, placeholder }) = &flag.state
            && !maps.iter().any(|(known, _)| known == term)
        {
            maps.push((term, placeholder));
        }
    }
    let mut message = resolved_line(&resolved, &bundle_path);
    if !maps.is_empty() {
        message.push_str(&offer(&mapping_file(mapping, env)?, &maps, learn).await?);
    }
    Ok(Report::success(message))
}

fn resolved_line(resolved: &capture::sanitize::Resolved, bundle: &Path) -> String {
    let (mut keep, mut redact, mut map) = (0, 0, 0);
    for (_, resolution) in &resolved.applied {
        *match resolution {
            Resolution::Keep => &mut keep,
            Resolution::Redact => &mut redact,
            Resolution::Map { .. } => &mut map,
        } += 1;
    }
    let rechecked = match resolved.rechecked.len() {
        0 => String::new(),
        count => format!(
            "the re-check that ingest runs redacted {count} {} that the marks exposed: {}\n",
            if count == 1 { "span" } else { "spans" },
            resolved
                .rechecked
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    };
    format!(
        "resolved {} {} in {} ({keep} keep, {redact} redact, {map} map); {}\n{rechecked}",
        keep + redact + map,
        if keep + redact + map == 1 {
            "flag"
        } else {
            "flags"
        },
        bundle.display(),
        match resolved.open {
            0 => "every flag is reviewed".to_owned(),
            1 => "1 flag still unreviewed".to_owned(),
            open => format!("{open} flags still unreviewed"),
        },
    )
}

async fn bundle_beside(report: &Path) -> Result<PathBuf, Error> {
    let name = report.display().to_string();
    let stem = name
        .strip_suffix(REPORT_SUFFIX)
        .ok_or_else(|| Error::ResolveBundle(name.clone()))?;
    let json = PathBuf::from(format!("{stem}.json"));
    if tokio::fs::try_exists(&json).await? {
        return Ok(json);
    }
    Ok(PathBuf::from(stem))
}

async fn offer(mapping: &MappingFile, maps: &[(&str, &str)], learn: bool) -> Result<String, Error> {
    let path = match mapping {
        MappingFile::Configured(path) | MappingFile::Default(path) => path,
    };
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::MappingRead {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let invalid = |source| Error::Mapping {
        path: path.display().to_string(),
        source,
    };
    let known = capture::sanitize::mapping::parse(&existing).map_err(invalid)?;
    let fresh: Vec<String> = maps
        .iter()
        .filter(|(term, _)| !known.learned(term))
        .map(|(term, placeholder)| capture::sanitize::mapping::map_rule(term, placeholder))
        .collect();
    let display = path.display();
    if fresh.is_empty() {
        return Ok(format!("{display} already has a rule for each map mark\n"));
    }
    let count = fresh.len();
    let noun = if count == 1 { "rule" } else { "rules" };
    let rules = fresh.join("\n");
    if !learn {
        return Ok(format!(
            "rerun with --learn to add {count} map {noun} to {display}:\n\n{rules}"
        ));
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&rules);
    capture::sanitize::mapping::parse(&text).map_err(invalid)?;
    write_whole(path.clone(), text.into_bytes()).await?;
    Ok(format!("added {count} map {noun} to {display}\n"))
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

const REPORT_SUFFIX: &str = ".report.md";

fn report_path(out: &Path) -> PathBuf {
    let base = if out.extension().is_some_and(|extension| extension == "json") {
        out.with_extension("")
    } else {
        out.to_path_buf()
    };
    let mut name = base.into_os_string();
    name.push(REPORT_SUFFIX);
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
            Ok(MemoryCommand::Sanitize(Job { input, out, mapping: Some(mapping), decide: false, block: false }))
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

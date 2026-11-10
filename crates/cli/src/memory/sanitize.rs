use std::path::Path;
use std::path::PathBuf;

use capture::sanitize::MarkState;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Resolution;
use capture::share;
use capture::share::BundlePaths;
use capture::share::Clobber;
use capture::share::ShareError;
use chrono::Utc;
use config::Environment;
use config::MappingFile;

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
    let decider = if decide {
        Some(
            share::decider(&super::target::load(env)?.unwrap_or_default(), block)
                .map_err(ShareError::from)?,
        )
    } else {
        None
    };
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(input).await?)?;
    let mapping = mapping_file(mapping, env)?;
    let paths = BundlePaths::beside(out);
    let outcome = share::sanitize(share::Job {
        bundle: &bundle,
        mapping: &mapping,
        key: &share::key_path(env)?,
        decider: decider.as_ref(),
        paths: &paths,
        clobber: Clobber::Replace,
        at: Utc::now(),
    })
    .await?;
    let decision = if let Some(decision) = &outcome.report.decision {
        decision_line(decision, &paths.report)
    } else {
        String::new()
    };
    let totals = outcome.report.totals();
    let count = bundle.sessions.len();
    let noun = if count == 1 { "session" } else { "sessions" };
    let mut out = format!(
        "sanitized {count} {noun}: {totals}; report at {}\n{decision}",
        paths.report.display()
    );
    if outcome.local {
        out.push_str("local review file at ");
        out.push_str(&paths.local.display().to_string());
        out.push_str(
            " holds the withheld text; never share it or the report, share only the bundle\n",
        );
    }
    out.push_str(&share::summary(&outcome.report));
    Ok(Report::success(out))
}

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
    let local_path = share::local_beside(report);
    let local: capture::sanitize::LocalReview =
        serde_json::from_slice(&tokio::fs::read(&local_path).await.map_err(|source| {
            Error::ReviewLocal {
                path: local_path.display().to_string(),
                source,
            }
        })?)?;
    let bundle = capture::bundle::parse(&tokio::fs::read_to_string(&bundle_path).await?)?;
    let key = capture::sanitize::LocalKey::load_or_create(&share::key_path(env)?)?;
    let resolved = capture::sanitize::resolve(&bundle, &flags, &local, &key)?;
    let mut maps: Vec<(&str, &str)> = Vec::new();
    for flag in &flags.flags {
        if let MarkState::Marked(Resolution::Map { term, placeholder })
        | MarkState::Resolved(Resolution::Map { term, placeholder }) = &flag.state
            && !maps.iter().any(|(known, _)| known == term)
        {
            maps.push((term, placeholder));
        }
    }
    let offered = if maps.is_empty() {
        None
    } else {
        Some(offer(&mapping_file(mapping, env)?, &maps, learn).await?)
    };
    if resolved.sealed.bundle != bundle {
        let mut sealed = resolved.sealed.text.clone();
        sealed.push(b'\n');
        share::write_file(bundle_path.clone(), sealed, Clobber::Replace).await?;
    }
    let applied: Vec<usize> = resolved.applied.iter().map(|(number, _)| *number).collect();
    let rewritten = flags.rewrite(&text, resolved.open, resolved.total, &applied);
    if rewritten != text {
        share::write_file(
            report.to_path_buf(),
            rewritten.into_bytes(),
            Clobber::Replace,
        )
        .await?;
    }
    let mut message = resolved_line(&resolved, &bundle_path);
    if let Some(offered) = offered {
        if let Some((path, text)) = offered.write {
            share::write_file(path, text.into_bytes(), Clobber::Replace).await?;
        }
        message.push_str(&offered.message);
    }
    Ok(Report::success(message))
}

struct Offered {
    message: String,
    write: Option<(PathBuf, String)>,
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
        .strip_suffix(share::REPORT_SUFFIX)
        .ok_or_else(|| Error::ResolveBundle(name.clone()))?;
    let json = PathBuf::from(format!("{stem}.json"));
    if tokio::fs::try_exists(&json).await? {
        return Ok(json);
    }
    Ok(PathBuf::from(stem))
}

async fn offer(
    mapping: &MappingFile,
    maps: &[(&str, &str)],
    learn: bool,
) -> Result<Offered, Error> {
    let path = match mapping {
        MappingFile::Configured(path) | MappingFile::Default(path) => path,
    };
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ShareError::MappingRead {
                path: path.display().to_string(),
                source,
            }
            .into());
        }
    };
    let invalid = |source| -> Error {
        ShareError::Mapping {
            path: path.display().to_string(),
            source,
        }
        .into()
    };
    let known = capture::sanitize::mapping::parse(&existing).map_err(invalid)?;
    let fresh: Vec<String> = maps
        .iter()
        .filter(|(term, _)| !known.learned(term))
        .map(|(term, placeholder)| capture::sanitize::mapping::map_rule(term, placeholder))
        .collect();
    let display = path.display();
    let told = |message: String| Offered {
        message,
        write: None,
    };
    if fresh.is_empty() {
        return Ok(told(format!(
            "{display} already has a rule for each map mark\n"
        )));
    }
    let count = fresh.len();
    let noun = if count == 1 { "rule" } else { "rules" };
    let rules = fresh.join("\n");
    if !learn {
        return Ok(told(format!(
            "rerun with --learn to add {count} map {noun} to {display}:\n\n{rules}"
        )));
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
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(Offered {
        message: format!("added {count} map {noun} to {display}\n"),
        write: Some((path.clone(), text)),
    })
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
}

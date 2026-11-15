use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::git;
use crate::git::git;
use crate::privacy;

#[derive(Debug, Default, Deserialize)]
pub struct Languages {
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

pub fn load(root: &Path) -> Result<Languages> {
    let text = std::fs::read_to_string(root.join("xtask").join("languages.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn glob_to_regex(glob: &str) -> Result<regex::Regex> {
    let mut pattern = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    pattern.push_str("(?:.*/)?");
                } else {
                    pattern.push_str(".*");
                }
            }
            '*' => pattern.push_str("[^/]*"),
            '?' => pattern.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                pattern.push('\\');
                pattern.push(ch);
            }
            other => pattern.push(other),
        }
    }
    pattern.push('$');
    Ok(regex::Regex::new(&pattern)?)
}

pub fn check(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let allow = load(root)?;
    let extension_matchers: Vec<regex::Regex> = allow
        .extensions
        .iter()
        .map(|ext| glob_to_regex(&format!("*.{ext}")))
        .collect::<Result<_>>()?;
    let glob_matchers: Vec<regex::Regex> = allow
        .globs
        .iter()
        .map(|glob| glob_to_regex(glob))
        .collect::<Result<_>>()?;
    let deny_matchers: Vec<regex::Regex> = allow
        .deny
        .iter()
        .map(|glob| glob_to_regex(glob))
        .collect::<Result<_>>()?;
    let files = git::tracked_files(root)?;
    for file in &files {
        if deny_matchers.iter().any(|matcher| matcher.is_match(file)) {
            findings.push(format!(
                "LANGUAGES {file}: denied by xtask/languages.toml (lane notes live on rivet-shared)"
            ));
            continue;
        }
        if allow.paths.iter().any(|path| path == file) {
            continue;
        }
        if extension_matchers
            .iter()
            .any(|matcher| matcher.is_match(file.rsplit('/').next().unwrap_or(file)))
        {
            continue;
        }
        if glob_matchers.iter().any(|matcher| matcher.is_match(file)) {
            continue;
        }
        findings.push(format!(
            "LANGUAGES {file}: not in the allow-list (xtask/languages.toml)"
        ));
    }
    Ok(files.len())
}

pub fn privacy_and_secrets(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let priv_deny = privacy::load_hashes(root, "privacy-denylist.json", findings)?;
    let sec_deny = privacy::load_hashes(root, "secret-denylist.json", findings)?;
    let files = git::tracked_files(root)?;
    let mut scanned = 0;
    for rel in &files {
        if privacy::skip_scan(rel) {
            continue;
        }
        let text = match std::fs::read_to_string(root.join(rel)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => return Err(Error::Io(error)),
        };
        privacy::scan_text(findings, &text, rel, &priv_deny, &sec_deny, None);
        scanned += 1;
    }
    Ok(scanned)
}

pub fn staging_text(root: &Path, file: &str) -> Result<Option<String>> {
    let text = git(root, &["show", &format!(":{file}")])?;
    if text.chars().take(4096).any(|c| c == '\u{fffd}') {
        return Ok(None);
    }
    Ok(Some(text))
}

pub fn added_lines(root: &Path, file: &str) -> Result<Vec<(usize, String)>> {
    let diff = git(
        root,
        &["diff", "--cached", "-U0", "--diff-filter=ACMR", "--", file],
    )?;
    let hunk = regex::Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)")?;
    let mut out = Vec::new();
    let mut n = 0usize;
    for line in diff.split('\n') {
        if let Some(caps) = hunk.captures(line)
            && let Some(value) = caps.get(1)
            && let Ok(value) = value.as_str().parse::<usize>()
        {
            n = value;
            continue;
        }
        if let Some(added) = line.strip_prefix('+')
            && !line.starts_with("+++")
        {
            out.push((n, added.to_owned()));
            n += 1;
        } else if !line.starts_with('-') && !line.starts_with('\\') {
            n += 1;
        }
    }
    Ok(out)
}

pub fn staged_privacy(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let priv_deny = privacy::load_hashes(root, "privacy-denylist.json", findings)?;
    let sec_deny = privacy::load_hashes(root, "secret-denylist.json", findings)?;
    let files = git::tracked_files(root)?;
    let staged = git(
        root,
        &["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
    )?;
    let mut count = 0;
    for rel in staged.lines().filter(|rel| !rel.is_empty()) {
        count += 1;
        if privacy::skip_scan(rel) {
            continue;
        }
        if !files.contains(&rel.to_owned()) {
            findings.push(format!(
                "LANGUAGES {rel}: not in the allow-list (xtask/languages.toml)"
            ));
        }
        if let Some(text) = staging_text(root, rel)?
            && rel.starts_with("crates/")
            && Path::new(rel)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        {
            crate::code::check_source(findings, rel, &text);
        }
        let added = added_lines(root, rel)?;
        if added.is_empty() {
            continue;
        }
        let numbers: Vec<usize> = added.iter().map(|(number, _)| *number).collect();
        let text: Vec<&str> = added.iter().map(|(_, line)| line.as_str()).collect();
        privacy::scan_text(
            findings,
            &text.join("\n"),
            rel,
            &priv_deny,
            &sec_deny,
            Some(&numbers),
        );
    }
    Ok(count)
}

pub fn ensure_git_root(path: &Path) -> Result<()> {
    let output = git(path, &["rev-parse", "--show-toplevel"])?;
    if output.trim().is_empty() {
        return Err(Error::Usage("not a git repository".to_owned()));
    }
    Ok(())
}

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::git;
use crate::rustsrc::Hit;

pub const FILE: &str = "xtask/baseline.toml";
const SOURCE: &str = "xtask/src/baseline.rs";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Entry {
    pub check: String,
    pub file: String,
    pub text: String,
    pub count: usize,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct Document {
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Debug, Clone, Default)]
pub struct Baseline {
    entries: Vec<Entry>,
}

impl Baseline {
    pub fn load(root: &Path) -> Result<Self> {
        match std::fs::read_to_string(root.join(FILE)) {
            Ok(text) => Ok(Self {
                entries: parse_entries(&text)?,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn known_check(&self, check: &str) -> bool {
        matches!(
            check,
            "FALLBACK" | "JSMODEL" | "JSONBYTES" | "UTF16" | "PORTS"
        )
    }

    pub fn count(&self, check: &str, file: &str, text: &str) -> usize {
        self.entries
            .iter()
            .find(|entry| entry.check == check && entry.file == file && entry.text == text)
            .map_or(0, |entry| entry.count)
    }
}

pub fn validate(baseline: &Baseline, findings: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for entry in baseline.entries() {
        if !baseline.known_check(&entry.check) {
            findings.push(format!(
                "BASELINE {FILE}: {} names no baseline-backed check",
                entry.check
            ));
        }
        if entry.file.is_empty() || entry.text.is_empty() || entry.count == 0 {
            findings.push(format!(
                "BASELINE {FILE}: entries require a file, matched text and positive count"
            ));
        }
        if !seen.insert((&entry.check, &entry.file, &entry.text)) {
            findings.push(format!(
                "BASELINE {FILE}: duplicate entry for {} {} {:?}",
                entry.check, entry.file, entry.text
            ));
        }
    }
}

pub fn adjudicate(baseline: &Baseline, check: &str, hits: &[Hit], findings: &mut Vec<String>) {
    let mut counts = BTreeMap::new();
    for hit in hits {
        let count = counts
            .entry((hit.file.as_str(), hit.text.as_str()))
            .or_insert(0usize);
        *count += 1;
        if *count > baseline.count(check, &hit.file, &hit.text) {
            findings.push(format!(
                "{check} {}:{}: {} (occurrence {})",
                hit.file, hit.line, hit.text, count
            ));
        }
    }
    for entry in baseline
        .entries()
        .iter()
        .filter(|entry| entry.check == check)
    {
        let actual = counts
            .get(&(entry.file.as_str(), entry.text.as_str()))
            .copied()
            .unwrap_or(0);
        if actual < entry.count {
            findings.push(format!("BASELINE {FILE}: {check} {} {:?} count {} no longer matches code ({actual}); reduce or remove the entry", entry.file, entry.text, entry.count));
        }
    }
}

pub fn compare(old: &[Entry], current: &[Entry], findings: &mut Vec<String>) {
    for entry in current {
        let prior = old.iter().find(|old| {
            old.check == entry.check && old.file == entry.file && old.text == entry.text
        });
        if prior.is_none_or(|prior| entry.count > prior.count) {
            findings.push(format!("BASELINE {FILE}: {} {} {:?} count {} was added or increased since origin/main; the baseline may only shrink", entry.check, entry.file, entry.text, entry.count));
        }
    }
}

pub fn shrink(root: &Path, findings: &mut Vec<String>) -> Result<()> {
    if let Some(base) = MergeBase::load(root)? {
        base.compare(Baseline::load(root)?.entries(), findings);
    }
    Ok(())
}

struct MergeBase {
    entries: Vec<Entry>,
    source: Option<String>,
}

impl MergeBase {
    fn load(root: &Path) -> Result<Option<Self>> {
        let Ok(base) = git(root, &["merge-base", "origin/main", "HEAD"]) else {
            return Ok(None);
        };
        let base = base.trim();
        let Ok(text) = git(root, &["show", &format!("{base}:{FILE}")]) else {
            return Ok(None);
        };
        Ok(Some(Self {
            entries: parse_entries(&text)?,
            source: git(root, &["show", &format!("{base}:{SOURCE}")]).ok(),
        }))
    }

    fn knows(&self, check: &str) -> bool {
        self.source
            .as_ref()
            .is_none_or(|source| source.contains(&format!("\"{check}\"")))
    }

    fn compare(&self, current: &[Entry], findings: &mut Vec<String>) {
        let known = current
            .iter()
            .filter(|entry| self.knows(&entry.check))
            .cloned()
            .collect::<Vec<_>>();
        compare(&self.entries, &known, findings);
    }
}

pub fn parse_entries(text: &str) -> Result<Vec<Entry>> {
    let document: Document = toml::from_str(text)?;
    Ok(document.entry)
}

pub fn write(root: &Path) -> Result<()> {
    let mut hits = crate::rustsrc::collect(root)?;
    hits.extend(
        crate::fallback::collect(root)?
            .into_iter()
            .map(|hit| ("FALLBACK", hit)),
    );
    write_hits(root, hits)
}

pub fn write_hits(root: &Path, hits: Vec<(&str, Hit)>) -> Result<()> {
    let empty = Baseline::default();
    let mut counts = BTreeMap::new();
    for (check, hit) in hits {
        if empty.known_check(check) {
            *counts
                .entry((check.to_owned(), hit.file, hit.text))
                .or_insert(0usize) += 1;
        }
    }
    let entry = counts
        .into_iter()
        .map(|((check, file, text), count)| Entry {
            check,
            file,
            text,
            count,
        })
        .collect();
    let document = Document { entry };
    let mut findings = Vec::new();
    if let Some(base) = MergeBase::load(root)? {
        base.compare(&document.entry, &mut findings);
    }
    if !findings.is_empty() {
        return Err(Error::Finding(findings.join("\n")));
    }
    std::fs::write(root.join(FILE), toml::to_string_pretty(&document)?)?;
    Ok(())
}

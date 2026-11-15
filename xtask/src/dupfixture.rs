use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::git;

pub fn check(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel in git::tracked_files(root)? {
        if owner(&rel).is_none() {
            continue;
        }
        let bytes = std::fs::read(root.join(&rel))?;
        groups.entry(digest(&bytes)).or_default().push(rel);
    }
    for paths in groups.values_mut() {
        paths.sort();
    }
    let mut count = 0;
    for paths in groups.values() {
        for (index, left) in paths.iter().enumerate() {
            let Some(left_owner) = owner(left) else {
                continue;
            };
            for right in paths.iter().skip(index + 1) {
                let Some(right_owner) = owner(right) else {
                    continue;
                };
                if left_owner == right_owner {
                    continue;
                }
                findings.push(format!("DUPFIXTURE {left}: bytes equal {right}"));
                count += 1;
            }
        }
    }
    Ok(count)
}

fn owner(rel: &str) -> Option<&str> {
    let rest = rel.strip_prefix("crates/")?;
    let (name, tail) = rest.split_once('/')?;
    let vendored = tail.starts_with("tests/golden/") || tail.starts_with("tests/fixtures/");
    vendored.then_some(name)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

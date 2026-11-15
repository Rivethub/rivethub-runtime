use std::path::Path;

use crate::error::{Error, Result};
use crate::git;

const LAB_PLANS: &str = concat!("/rivet-shared", "/plans/");

pub fn check(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let mut count = 0;
    for rel in git::tracked_files(root)? {
        if !scanned(&rel) {
            continue;
        }
        let text = match std::fs::read_to_string(root.join(&rel)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => return Err(Error::Io(error)),
        };
        if text.contains(LAB_PLANS) {
            findings.push(format!(
                "LABPATH {rel}: names the lab plans tree; tests read the vendored recordings under crates/testkit/tests/fixtures"
            ));
            count += 1;
        }
    }
    Ok(count)
}

fn scanned(rel: &str) -> bool {
    (rel.starts_with("crates/") || rel.starts_with("xtask/")) && !recorded(rel)
}

fn recorded(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix("crates/") else {
        return false;
    };
    let Some((_, tail)) = rest.split_once('/') else {
        return false;
    };
    tail.starts_with("tests/golden/") || tail.starts_with("tests/fixtures/")
}

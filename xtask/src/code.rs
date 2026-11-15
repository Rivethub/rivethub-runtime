use std::path::Path;

use ra_ap_rustc_lexer::{Cursor, FrontmatterAllowed, TokenKind};

use crate::error::Result;
use crate::git;

pub const FILE_CEILING: usize = 600;

pub fn comments(text: &str) -> Vec<(usize, String)> {
    let mut cursor = Cursor::new(text, FrontmatterAllowed::No);
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let token = cursor.advance_token();
        if token.kind == TokenKind::Eof {
            break;
        }
        let line_number = text[..offset].matches('\n').count() + 1;
        if matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
        ) {
            let end = text[offset..]
                .find('\n')
                .map_or(text.len(), |index| offset + index);
            let snippet = comment_text(text, offset, end);
            out.push((line_number, snippet));
        }
        offset += token.len as usize;
    }
    out
}

fn comment_text(text: &str, offset: usize, end: usize) -> String {
    text[offset..end].trim().to_owned()
}

pub fn check_source(findings: &mut Vec<String>, rel: &str, text: &str) {
    for (line_number, snippet) in comments(text) {
        findings.push(format!("COMMENT {rel}:{line_number}: comment ({snippet})"));
    }
}

fn is_rust(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

pub fn check_code(root: &Path, findings: &mut Vec<String>) -> Result<usize> {
    let files = git::tracked_files(root)?;
    let mut count = 0;
    for rel in &files {
        if !rel.starts_with("crates/") || !is_rust(rel) {
            continue;
        }
        let bytes = std::fs::read(root.join(rel))?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            findings.push(format!(
                "UTF8 {rel}: not valid UTF-8; the Rust lexer cannot read the file"
            ));
            continue;
        };
        let text = text.to_owned();
        check_source(findings, rel, &text);
        if is_naming_violation(rel) {
            findings.push(format!("NAMING {rel}: den/room file or module name"));
        }
        if !format!("/{rel}").contains("/tests/") {
            let non_blank = text.lines().filter(|line| !line.trim().is_empty()).count();
            if non_blank > FILE_CEILING {
                findings.push(format!(
                    "SIZE {rel}: {non_blank} lines (ceiling {FILE_CEILING})"
                ));
            }
        }
        count += 1;
    }
    Ok(count)
}

fn is_naming_violation(rel: &str) -> bool {
    rel.split('/').any(|segment| {
        let stem = segment.strip_suffix(".rs").unwrap_or(segment);
        matches!(stem, "den" | "room" | "rooms")
    })
}

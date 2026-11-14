use std::collections::BTreeMap;

use crate::memory::Provenance;
use crate::memory::Verification;
use crate::wiki::WikiCitation;

const SUMMARY_COLUMN: usize = 3;

#[must_use]
pub fn imported_banner<'a>(imported: impl IntoIterator<Item = &'a Provenance>) -> Option<String> {
    let mut authors: Vec<(&str, Verification)> = Vec::new();
    for provenance in imported {
        let author = provenance.author_label();
        match authors.iter_mut().find(|(seen, _)| *seen == author) {
            Some((_, verification)) if verification.is_verified() => {
                *verification = provenance.verification;
            }
            Some(_) => {}
            None => authors.push((author, provenance.verification)),
        }
    }
    if authors.is_empty() {
        return None;
    }
    let named: Vec<String> = authors
        .iter()
        .map(|(author, verification)| format!("{author} ({})", verification.short()))
        .collect();
    Some(format!(
        "Built partly from imported sessions: {}.",
        named.join(", ")
    ))
}

#[must_use]
pub fn mark_imported_citations(view: &str, citations: &[WikiCitation]) -> String {
    let marks: BTreeMap<String, String> = citations
        .iter()
        .filter_map(|citation| {
            let provenance = citation.imported.as_ref()?;
            let mark = format!(
                "imported from {}, {}",
                provenance.author_label().replace('|', "\\|"),
                provenance.verification.short()
            );
            Some((format!("`{}`", citation.summary_id), mark))
        })
        .collect();
    if marks.is_empty() {
        return view.to_owned();
    }
    let mut marked = String::with_capacity(view.len());
    for line in view.split_inclusive('\n') {
        marked.push_str(&mark_row(line, &marks));
    }
    marked
}

fn mark_row(line: &str, marks: &BTreeMap<String, String>) -> String {
    let body = line.trim_end_matches(['\n', '\r']);
    let ending = line.get(body.len()..).unwrap_or_default();
    let mark = body
        .strip_prefix('|')
        .and_then(|_| body.split('|').nth(SUMMARY_COLUMN))
        .and_then(|cell| marks.get(cell.trim()));
    let row = body.trim_end().strip_suffix('|');
    match (mark, row) {
        (Some(mark), Some(row)) => format!("{} ({mark}) |{ending}", row.trim_end()),
        _ => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(author: &str, verified: bool) -> Provenance {
        Provenance {
            team_owner: None,
            author: Some(author.to_owned()),
            origin_date: None,
            verification: if verified {
                Verification::Verified
            } else {
                Verification::Unsigned
            },
            checks: None,
        }
    }

    fn citation(summary: &str, imported: Option<Provenance>) -> WikiCitation {
        WikiCitation {
            summary_id: summary.to_owned(),
            date: None,
            kind: None,
            note: None,
            conversation_id: None,
            session_id: None,
            title: None,
            channel: None,
            topic_id: None,
            imported,
        }
    }

    #[test]
    fn the_banner_names_each_author_once_and_keeps_the_weakest_verification() {
        let phil = provenance("phil", true);
        let weak = provenance("phil", false);
        let alex = provenance("alex", true);
        assert_eq!(
            imported_banner([&phil, &alex, &weak]).as_deref(),
            Some("Built partly from imported sessions: phil (unverified), alex (verified).")
        );
        assert_eq!(imported_banner([]), None);
    }

    #[test]
    fn only_the_rows_citing_an_import_are_marked() {
        let view = "## Citations\n\n| Date | Kind | Summary | Note |\n|---|---|---|---|\n| 2026-10-14 | leaf | `aaa` | Decision \\| made |\n| 2026-10-15 | leaf | `bbb` |  |\n| 2026-10-16 | leaf | `ccc` | Mine |\nMentions `aaa` in prose.\n";
        let citations = [
            citation("aaa", Some(provenance("phil", false))),
            citation("bbb", Some(provenance("alex", true))),
            citation("ccc", None),
        ];
        assert_eq!(
            mark_imported_citations(view, &citations),
            "## Citations\n\n| Date | Kind | Summary | Note |\n|---|---|---|---|\n| 2026-10-14 | leaf | `aaa` | Decision \\| made (imported from phil, unverified) |\n| 2026-10-15 | leaf | `bbb` | (imported from alex, verified) |\n| 2026-10-16 | leaf | `ccc` | Mine |\nMentions `aaa` in prose.\n"
        );
        assert_eq!(
            mark_imported_citations(view, &[citation("ccc", None)]),
            view
        );
    }
}

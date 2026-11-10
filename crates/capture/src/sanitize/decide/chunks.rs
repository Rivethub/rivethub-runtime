use std::ops::Range;

use wire::capture::BundledSession;

use super::texts::At;

pub const CHUNK_CHARS: usize = 1200;
const SCORED_LENGTH: usize = 16;
const TOOL_CALL: &str = "[tool call] ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub(super) owner: String,
    pub(super) at: At,
    pub(super) range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub chunks: Vec<Chunk>,
    pub trivial: usize,
}

pub(crate) fn candidates(sessions: &mut [BundledSession]) -> Candidates {
    let mut chunks = Vec::new();
    let mut trivial = 0;
    super::texts::visit(sessions, &mut |slot, text| {
        for range in split(text) {
            let piece = &text[range.clone()];
            if is_trivial(piece) {
                trivial += 1;
                continue;
            }
            chunks.push(Chunk {
                text: piece.to_owned(),
                owner: slot.owner.to_owned(),
                at: slot.at.clone(),
                range,
            });
        }
    });
    Candidates { chunks, trivial }
}

fn is_trivial(text: &str) -> bool {
    if text
        .strip_prefix(TOOL_CALL)
        .is_some_and(|name| !name.is_empty() && !name.contains(char::is_whitespace))
    {
        return true;
    }
    if text
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '{' | '}'))
    {
        return false;
    }
    if only_placeholders(text) {
        return true;
    }
    !(text.chars().count() >= SCORED_LENGTH
        || text.contains(['@', '/', '=', ',', ':', ';'])
        || has_capitalized_word(text))
}

fn only_placeholders(text: &str) -> bool {
    let names = crate::rules::ruleset().names();
    let mut rest = text;
    while !rest.is_empty() {
        let length = names.placeholder_len(rest);
        if length == 0 {
            return false;
        }
        rest = rest.get(length..).unwrap_or_default();
    }
    !text.is_empty()
}

fn has_capitalized_word(text: &str) -> bool {
    text.split(|c: char| !c.is_alphabetic())
        .any(|word| word.chars().count() >= 2 && word.chars().any(char::is_uppercase))
}

fn split(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut chars = 0;
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let length = line.chars().count();
        if chars > 0 && chars + length > CHUNK_CHARS {
            ranges.push(start..end);
            start = end;
            chars = 0;
        }
        if length > CHUNK_CHARS {
            let mut cut = end;
            for (count, (offset, _)) in line.char_indices().enumerate() {
                if count > 0 && count % CHUNK_CHARS == 0 {
                    ranges.push(cut..end + offset);
                    cut = end + offset;
                }
            }
            start = cut;
            chars = line[cut - end..].chars().count();
        } else {
            chars += length;
        }
        end += line.len();
    }
    if chars > 0 {
        ranges.push(start..end);
    }
    ranges
        .into_iter()
        .filter_map(|range| {
            let piece = text[range.clone()].trim_end_matches(['\n', '\r']);
            (!piece.trim().is_empty()).then(|| range.start..range.start + piece.len())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str) -> Vec<&str> {
        split(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn chunks_pack_whole_lines_up_to_the_limit() {
        let line = format!("{}\n", "a".repeat(700));
        let text = format!("{line}{line}{line}");
        assert_eq!(
            pieces(&text),
            vec![&line[..700], &line[..700], &line[..700]]
        );
        assert_eq!(pieces("one\ntwo\n"), vec!["one\ntwo"]);
        assert_eq!(pieces("\n  \n"), Vec::<&str>::new());
    }

    #[test]
    fn a_line_over_the_limit_is_cut_at_char_boundaries() {
        let text = "é".repeat(CHUNK_CHARS * 2 + 5);
        let cut = pieces(&text);
        assert_eq!(
            cut.iter()
                .map(|piece| piece.chars().count())
                .collect::<Vec<_>>(),
            vec![CHUNK_CHARS, CHUNK_CHARS, 5]
        );
        assert_eq!(cut.concat(), text);
    }

    #[test]
    fn short_bare_tokens_are_trivial_and_names_and_pairs_are_not() {
        for trivial in [
            "true",
            "42",
            "x86_64",
            "[REDACTED:secret_key]",
            "[REDACTED:secret_key][FLAGGED:3]",
            "[tool call] Bash",
            "[tool call] mcp__rivethub__memory_search",
            "",
        ] {
            assert!(is_trivial(trivial), "{trivial}");
        }
        for scored in [
            "Initech ledger",
            "Globex",
            "PostToolUse",
            "margot.vellacourt@initech.com",
            "salary=120k",
            "a,b,c",
            "0f3c2a9e-6d1b-4c55-9a0e-2b7d8f1c4e60",
            "/home/x",
            r#"{"customer":"Initech"}"#,
            "it's",
            "[tool call] two words",
        ] {
            assert!(!is_trivial(scored), "{scored}");
        }
    }
}

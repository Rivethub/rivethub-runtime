use std::ops::Range;

use wire::capture::BundledSession;

use super::slots::FieldName;

const CHUNK_CHARS: usize = 1200;
const SCORED_LENGTH: usize = 16;
const MIXED_LENGTH: usize = 5;
const PHONE_DIGITS: usize = 7;
const HEX_ID: usize = 12;
const BASE64_ID: usize = 20;
const TOOL_CALL: &str = "[tool call] ";
const ID_KEYS: [&str; 10] = [
    "uuid",
    "parentuuid",
    "leafuuid",
    "requestid",
    "sessionid",
    "messageid",
    "tooluseid",
    "promptid",
    "sourcetooluseid",
    "sessionjsonlpath",
];
const ID_SUFFIXES: [&str; 5] = ["_id", "_uuid", "_path", "Id", "Uuid"];
const FILE_EXTENSIONS: [&str; 24] = [
    "rs", "md", "py", "js", "ts", "tsx", "jsx", "json", "toml", "yaml", "yml", "txt", "lock", "sh",
    "go", "rb", "java", "c", "h", "cpp", "html", "css", "log", "sql",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub(super) session: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub chunks: Vec<Chunk>,
    pub trivial: usize,
}

pub(crate) fn candidates(sessions: &mut [BundledSession]) -> Candidates {
    let mut chunks = Vec::new();
    let mut trivial = 0;
    super::slots::scored(sessions, &mut |at, path, text| {
        let key = path.rsplit(['.', '[']).next().unwrap_or_default();
        let id_field = at.name == FieldName::Metadata && is_id_key(key);
        for range in split(text) {
            let piece = &text[range];
            if id_field || is_trivial(piece) || (is_id_suffixed(key) && is_identifier(piece)) {
                trivial += 1;
                continue;
            }
            chunks.push(Chunk {
                text: piece.to_owned(),
                session: at.session,
            });
        }
    });
    Candidates { chunks, trivial }
}

fn is_id_key(key: &str) -> bool {
    let folded: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    ID_KEYS.contains(&folded.as_str())
}

fn is_id_suffixed(key: &str) -> bool {
    ID_SUFFIXES.iter().any(|suffix| key.ends_with(suffix))
}

fn is_identifier(token: &str) -> bool {
    let stem = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .split('.')
        .next()
        .unwrap_or(token);
    is_bare_id(token) || (token.contains('/') && is_bare_id(stem))
}

fn is_bare_id(token: &str) -> bool {
    uuid::Uuid::try_parse(token).is_ok()
        || is_hex_id(token)
        || is_base64_id(token)
        || is_time(token)
}

fn is_hex_id(token: &str) -> bool {
    token.len() >= HEX_ID
        && token.bytes().all(|b| b.is_ascii_hexdigit())
        && token.bytes().any(|b| b.is_ascii_digit())
        && token.bytes().any(|b| b.is_ascii_alphabetic())
}

fn is_base64_id(token: &str) -> bool {
    token.len() >= BASE64_ID
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'='))
        && token.bytes().any(|b| b.is_ascii_digit())
        && token.bytes().any(|b| b.is_ascii_uppercase())
        && token.bytes().any(|b| b.is_ascii_lowercase())
}

fn is_time(token: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(token).is_ok()
        || chrono::NaiveDate::parse_from_str(token, "%Y-%m-%d").is_ok()
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
    if only_placeholders(text) || is_bare_id(text) {
        return true;
    }
    !(text.chars().count() >= SCORED_LENGTH
        || text.contains(['@', '/', '=', ',', ':', ';'])
        || has_capitalized_word(text)
        || is_sensitive_shape(text))
}

fn is_sensitive_shape(token: &str) -> bool {
    let letters = token.chars().any(char::is_alphabetic);
    let digits = token.chars().filter(char::is_ascii_digit).count();
    token.chars().any(|c| c.is_alphabetic() && !c.is_ascii())
        || (letters && digits > 0 && token.chars().count() >= MIXED_LENGTH)
        || is_ipv4(token)
        || is_phone(token, digits)
        || is_hostname(token)
}

fn is_ipv4(token: &str) -> bool {
    let groups: Vec<&str> = token.split('.').collect();
    groups.len() == 4
        && groups
            .iter()
            .all(|group| (1..=3).contains(&group.len()) && group.parse::<u8>().is_ok())
}

fn is_phone(token: &str, digits: usize) -> bool {
    digits >= PHONE_DIGITS
        && (token.starts_with('+') || token.contains('-'))
        && token
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | '.'))
}

fn is_hostname(token: &str) -> bool {
    let labels: Vec<&str> = token.split('.').collect();
    let Some(last) = labels.last() else {
        return false;
    };
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && (2..=6).contains(&last.len())
        && last.chars().all(|c| c.is_ascii_lowercase())
        && !FILE_EXTENSIONS.contains(last)
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
    fn short_bare_tokens_are_trivial_and_names_pairs_and_secrets_are_not() {
        for trivial in [
            "true",
            "42",
            "v1",
            "[REDACTED:secret_key]",
            "[REDACTED:secret_key][FLAGGED:3]",
            "[tool call] Bash",
            "[tool call] mcp__rivethub__memory_search",
            "0f3c2a9e-6d1b-4c55-9a0e-2b7d8f1c4e60",
            "3f9a2c1be07d4e5f",
            "req_011CVzKq8RmT2vLx9WabQ7",
            "2026-11-03T10:24:51Z",
            "2026-11-03",
            "main.rs",
            "margot",
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
            "/home/x",
            r#"{"customer":"Initech"}"#,
            "it's",
            "[tool call] two words",
            "10.4.20.207",
            "078-05-1120",
            "+15558675309",
            "hunter2",
            "P4ssw0rd!",
            "山田太郎",
            "müller",
            "gpubox.lan",
            "sk-live-9f8e7d",
        ] {
            assert!(!is_trivial(scored), "{scored}");
        }
    }

    #[test]
    fn identifier_fields_are_skipped_by_name_and_value() {
        assert!(is_id_key("parentUuid") && is_id_key("session_jsonl_path") && is_id_key("uuid"));
        assert!(!is_id_key("note"));
        assert!(is_identifier(
            "~/.claude/projects/-home-phil-work/6b0404f2-b094-90b8-6b01-a1c12a3a2107.jsonl"
        ));
        assert!(!is_identifier(
            "~/clients/beaumont-family-trust/tax-2025.pdf"
        ));
    }
}

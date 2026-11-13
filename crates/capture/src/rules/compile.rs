use regex::Regex;
use regex::RegexBuilder;

const REGEX_SIZE_LIMIT: usize = 32 << 20;

pub(super) fn compile(source: &str) -> Result<Regex, regex::Error> {
    let source = rust_syntax(source);
    RegexBuilder::new(&source)
        .unicode(false)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .or_else(|_| {
            RegexBuilder::new(&source)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
        })
}

pub(super) fn compile_allow(source: &str) -> Result<Regex, regex::Error> {
    compile(&anchor_alternation(source))
}

fn rust_syntax(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len() + 8);
    let mut in_class = false;
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        match c {
            '\\' => {
                out.push(c);
                if let Some(&next) = chars.get(index + 1) {
                    out.push(next);
                    index += 1;
                    if matches!(next, 'x' | 'u' | 'U' | 'p' | 'P')
                        && chars.get(index + 1) == Some(&'{')
                    {
                        while let Some(&inner) = chars.get(index + 1) {
                            out.push(inner);
                            index += 1;
                            if inner == '}' {
                                break;
                            }
                        }
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            '{' if !in_class && !repetition_follows(&chars, index) => out.push_str("\\{"),
            '}' if !in_class && !repetition_closes(&chars, index) => out.push_str("\\}"),
            _ => out.push(c),
        }
        index += 1;
    }
    out
}

fn repetition_follows(chars: &[char], open: usize) -> bool {
    let mut index = open + 1;
    let mut digits = 0;
    while chars.get(index).is_some_and(char::is_ascii_digit) {
        index += 1;
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if chars.get(index) == Some(&',') {
        index += 1;
        while chars.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
    }
    chars.get(index) == Some(&'}')
}

fn repetition_closes(chars: &[char], close: usize) -> bool {
    let mut index = close;
    while index > 0 {
        index -= 1;
        match chars.get(index) {
            Some(c) if c.is_ascii_digit() || *c == ',' => {}
            Some('{') => return repetition_follows(chars, index),
            _ => return false,
        }
    }
    false
}

fn anchor_alternation(source: &str) -> String {
    let (flags, body) = match source.strip_prefix("(?") {
        Some(rest) if !rest.starts_with(':') => match rest.find(')') {
            Some(close) => source.split_at(close + 3),
            None => ("", source),
        },
        _ => ("", source),
    };
    let Some(inner) = body
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
    else {
        return source.to_owned();
    };
    if !has_top_level_alternation(inner) {
        return source.to_owned();
    }
    format!("{flags}^(?:{inner})$")
}

fn has_top_level_alternation(source: &str) -> bool {
    let mut depth = 0usize;
    let mut in_class = false;
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re2_literal_braces_become_rust_escapes() {
        assert_eq!(rust_syntax(r"^\$(?:\d+|{\d+})$"), r"^\$(?:\d+|\{\d+\})$");
        assert_eq!(rust_syntax(r"{{[^}]+}}"), r"\{\{[^}]+\}\}");
        assert_eq!(rust_syntax(r"a{2,3}b{4}c{5,}\{d"), r"a{2,3}b{4}c{5,}\{d");
        assert_eq!(
            rust_syntax(r"[\x{200B}-\x{200D}]\p{L}{2}"),
            r"[\x{200B}-\x{200D}]\p{L}{2}"
        );
    }

    #[test]
    fn an_anchored_alternation_is_grouped_so_the_anchors_cover_every_branch() {
        assert_eq!(
            anchor_alternation("(?i)^true|false|null$"),
            "(?i)^(?:true|false|null)$"
        );
        assert_eq!(anchor_alternation("^a|b$"), "^(?:a|b)$");
        assert_eq!(anchor_alternation("^(?:a|b)$"), "^(?:a|b)$");
        assert_eq!(anchor_alternation("^[a|b]$"), "^[a|b]$");
        assert_eq!(anchor_alternation("a|b"), "a|b");
        assert!(
            compile_allow("(?i)^true|false|null$")
                .is_ok_and(|re| !re.is_match("PLANTEDfalse9Z3k") && re.is_match("FALSE"))
        );
    }
}

use std::ops::Range;

const BARE_END: &[char] = &[',', ';', '&', ')', ']', '}', '"', '\''];
const OPENERS: &[(&str, &str)] = &[
    ("${{", "}}"),
    ("${", "}"),
    ("$(", ")"),
    ("{{", "}}"),
    ("{", "}"),
    ("[", "]"),
    ("(", ")"),
    ("<", ">"),
    ("%", "%"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    Quoted,
    Bare,
    Bracketed,
}

#[derive(Debug)]
pub(super) struct Value {
    pub(super) range: Range<usize>,
    pub(super) shape: Shape,
}

pub(super) fn parse(rest: &str, placeholder_len: &dyn Fn(&str) -> usize) -> Option<Value> {
    let line_end = rest.find(['\n', '\r']).unwrap_or(rest.len());
    let line = rest.get(..line_end)?;
    for opener in ["\\\"", "\\'", "\"", "'"] {
        if let Some(inner) = line.strip_prefix(opener) {
            let start = opener.len();
            let end = closing_quote(inner, opener).map_or(line_end, |close| start + close);
            let start = start + placeholder_len(line.get(start..end)?);
            return (end > start).then_some(Value {
                range: start..end,
                shape: Shape::Quoted,
            });
        }
    }
    if is_block_scalar(line.trim_end()) {
        return block_body(rest, line_end);
    }
    if line.trim().is_empty() {
        return next_line_value(rest, line_end, placeholder_len);
    }
    let skipped = placeholder_len(line);
    if skipped > 0 {
        let end = skipped + bare_end(line.get(skipped..)?);
        return (end > skipped).then_some(Value {
            range: skipped..end,
            shape: Shape::Bare,
        });
    }
    if let Some((open, close)) = OPENERS.iter().find(|(open, _)| line.starts_with(open))
        && let Some(end) = balanced_end(line, open, close)
    {
        return Some(Value {
            range: 0..end,
            shape: Shape::Bracketed,
        });
    }
    let end = bare_end(line);
    (end > 0).then_some(Value {
        range: 0..end,
        shape: Shape::Bare,
    })
}

fn next_line_value(
    rest: &str,
    line_end: usize,
    placeholder_len: &dyn Fn(&str) -> usize,
) -> Option<Value> {
    let mut at = line_end;
    loop {
        let tail = rest.get(at..)?;
        let newline = tail.find('\n')?;
        at += newline + 1;
        let next = rest.get(at..)?;
        let next_end = at + next.find(['\n', '\r']).unwrap_or(next.len());
        let line = rest.get(at..next_end)?;
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            return None;
        }
        let indent = line.len() - line.trim_start().len();
        let start = at + indent;
        let body = rest.get(start..next_end)?;
        if body.starts_with(['"', '\'']) {
            let inner = parse(body, placeholder_len)?;
            return Some(Value {
                range: start + inner.range.start..start + inner.range.end,
                shape: inner.shape,
            });
        }
        let single_token = !body.trim_end().contains(char::is_whitespace)
            && !body.trim_end().ends_with([':', '=']);
        return single_token.then(|| Value {
            range: start..start + body.trim_end().len(),
            shape: Shape::Bare,
        });
    }
}

pub(super) fn prose_follows(rest: &str, value_end: usize) -> bool {
    let tail = rest.get(value_end..).unwrap_or_default();
    let line = tail.split(['\n', '\r']).next().unwrap_or_default();
    line.starts_with([' ', '\t'])
        && line
            .trim_start()
            .contains(|c: char| c.is_ascii_alphabetic())
}

fn is_block_scalar(indicator: &str) -> bool {
    indicator
        .strip_prefix(['|', '>'])
        .is_some_and(|chomping| matches!(chomping, "" | "-" | "+"))
}

fn block_body(rest: &str, line_end: usize) -> Option<Value> {
    let mut at = line_end;
    let mut start = None;
    let mut end = line_end;
    while let Some(newline) = rest.get(at..)?.find('\n') {
        at += newline + 1;
        let tail = rest.get(at..)?;
        let next_end = at + tail.find(['\n', '\r']).unwrap_or(tail.len());
        let line = rest.get(at..next_end)?;
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            break;
        }
        let indent = line.len() - line.trim_start().len();
        start.get_or_insert(at + indent);
        end = at + line.trim_end().len();
    }
    start.map(|start| Value {
        range: start..end,
        shape: Shape::Quoted,
    })
}

fn balanced_end(line: &str, open: &str, close: &str) -> Option<usize> {
    if open.len() > 1 || close.len() > 1 || !matches!(open, "{" | "[" | "(") {
        return line
            .get(open.len()..)
            .and_then(|tail| tail.find(close))
            .map(|found| open.len() + found + close.len());
    }
    let (open_char, close_char) = (open.chars().next()?, close.chars().next()?);
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == active {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == open_char {
            depth += 1;
        } else if c == close_char {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index + c.len_utf8());
            }
        }
    }
    None
}

pub(super) fn bare_end(text: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (index, c) in text.char_indices() {
        if let Some(active) = quote {
            if c == active {
                quote = None;
            }
            continue;
        }
        if depth == 0
            && (c.is_whitespace()
                || (BARE_END.contains(&c) && !(index == 0 && reference_follows(text))))
        {
            return index;
        }
        if c == '\\'
            && text
                .get(index + 1..)
                .is_some_and(|next| next.starts_with(['n', 'r', 't']))
        {
            return index;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    text.len()
}

fn reference_follows(text: &str) -> bool {
    text.strip_prefix('&').is_some_and(|rest| {
        let end = rest
            .find(|c: char| c.is_whitespace() || BARE_END.contains(&c))
            .unwrap_or(rest.len());
        rest.get(..end)
            .is_some_and(|word| !word.is_empty() && !word.contains('='))
    })
}

pub(super) fn unwrap_brackets(value: &str) -> &str {
    OPENERS
        .iter()
        .find_map(|(open, close)| {
            value
                .strip_prefix(open)
                .and_then(|inner| inner.strip_suffix(close))
        })
        .map_or(value, str::trim)
}

pub(super) fn inner_strings(value: &str) -> Option<Vec<Range<usize>>> {
    let body = value.trim_start_matches(['{', '[', ' ', '\t']);
    if !(value.starts_with('{') || value.starts_with('[')) || !body.starts_with(['"', '{', '[']) {
        return None;
    }
    let mut strings = Vec::new();
    let mut open: Option<usize> = None;
    let mut escaped = false;
    for (index, c) in value.char_indices() {
        match open {
            Some(start) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    let after = value.get(index + 1..).unwrap_or_default().trim_start();
                    if !after.starts_with(':') {
                        strings.push(start..index);
                    }
                    open = None;
                }
            }
            None if c == '"' => open = Some(index + 1),
            None => {}
        }
    }
    Some(strings)
}

fn closing_quote(inner: &str, opener: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = inner.get(from..)?.find(opener) {
        let at = from + found;
        let escaped = opener.len() == 1 && inner.get(..at).is_some_and(|head| head.ends_with('\\'));
        if !escaped {
            return Some(at);
        }
        from = at + opener.len();
    }
    None
}

pub(super) fn normalize(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let chars: Vec<char> = key.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        let previous = index.checked_sub(1).and_then(|at| chars.get(at));
        let next = chars.get(index + 1);
        let boundary = c.is_uppercase()
            && previous.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            });
        if boundary && !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out.trim_matches('_').to_owned()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a secret is far shorter than 2^52 bytes, so the counts convert exactly"
)]
#[must_use]
pub(super) fn shannon_entropy(text: &str) -> f64 {
    let mut counts = [0usize; 256];
    for byte in text.bytes() {
        if let Some(slot) = counts.get_mut(usize::from(byte)) {
            *slot += 1;
        }
    }
    let total = text.len() as f64;
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let p = count as f64 / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_values_run_to_the_balanced_end() {
        assert_eq!(bare_end("os.environ[\"X\"] rest"), 15);
        assert_eq!(bare_end("get(\"a, b\"),x"), 11);
        assert_eq!(bare_end("abc;def"), 3);
        assert_eq!(bare_end("abc"), 3);
        assert_eq!(bare_end("Option<&str>,"), 12);
        assert_eq!(bare_end("&str,"), 4);
        assert_eq!(bare_end("x&y=z"), 1);
        assert_eq!(bare_end("&token_type_hint=refresh"), 0);
        assert_eq!(bare_end("x\\nnot_a_key"), 1);
    }

    #[test]
    fn unwrapping_removes_one_bracket_pair_only() {
        assert_eq!(
            unwrap_brackets("{searchParams.get('q')}"),
            "searchParams.get('q')"
        );
        assert_eq!(unwrap_brackets("${{ secrets.X }}"), "secrets.X");
        assert_eq!(unwrap_brackets("(pwd)"), "pwd");
        assert_eq!(unwrap_brackets("plain"), "plain");
    }

    #[test]
    fn json_containers_expose_their_string_values_only() {
        let strings = inner_strings(r#"{"value": "x", "sensitive": true, "list": ["y"]}"#)
            .unwrap_or_default();
        assert_eq!(strings.len(), 2);
        assert!(inner_strings("{pwPLANTEDa1}").is_none());
        assert!(inner_strings("(a)").is_none());
    }

    #[test]
    fn keys_normalize_to_snake_case_segments() {
        assert_eq!(normalize("X-Api-Key"), "x_api_key");
        assert_eq!(normalize("clientSecret"), "client_secret");
        assert_eq!(normalize("APIKey"), "api_key");
        assert_eq!(normalize("_authToken"), "auth_token");
        assert_eq!(normalize("secretName"), "secret_name");
        assert_eq!(normalize("x.auth.y"), "x_auth_y");
        assert_eq!(normalize("PGPASSWORD"), "pgpassword");
    }

    #[test]
    fn entropy_grows_with_variety() {
        assert!(shannon_entropy("aaaaaaaa") < 0.01);
        assert!(shannon_entropy("abcdefgh") > 2.9);
        assert!(shannon_entropy("Zq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA1e") > 4.5);
    }
}

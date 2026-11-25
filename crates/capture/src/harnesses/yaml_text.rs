use std::collections::BTreeSet;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum EditError {
    #[error("`{0}` is not a block map rh can edit in place")]
    NotABlockMap(String),
}

const DEFAULT_INDENT: usize = 2;

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn has_content(line: &str) -> bool {
    !is_blank(line) && !is_comment(line)
}

fn key_line(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\r', '\n']))
}

fn child_name(line: &str) -> Option<String> {
    let text = line.trim();
    if text == "-" || text.starts_with("- ") || text.starts_with("? ") {
        return None;
    }
    if let Some(quote) = text.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let (name, rest) = text.strip_prefix(quote)?.split_once(quote)?;
        return rest.trim_start().starts_with(':').then(|| name.to_owned());
    }
    text.match_indices(':').find_map(|(at, _)| {
        let (head, tail) = text.split_at_checked(at + 1)?;
        let separated = tail.is_empty() || tail.starts_with([' ', '\t']);
        separated.then(|| head.trim_end_matches(':').to_owned())
    })
}

enum Value {
    Block,
    Empty,
    Other,
}

fn classify(line: &str, key: &str) -> (Value, String) {
    let rest = line
        .strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n']);
    let (value, comment) = rest
        .match_indices('#')
        .find_map(|(at, _)| {
            let (head, tail) = rest.split_at_checked(at)?;
            (head.is_empty() || head.ends_with([' ', '\t'])).then_some((head, tail))
        })
        .unwrap_or((rest, ""));
    let comment = if comment.is_empty() {
        String::new()
    } else {
        format!(" {comment}")
    };
    match value.trim() {
        "" => (Value::Block, comment),
        "{}" | "null" | "Null" | "NULL" | "~" => (Value::Empty, comment),
        _ => (Value::Other, comment),
    }
}

struct Section {
    key: usize,
    end: usize,
    child_indent: Option<usize>,
}

fn section(lines: &[&str], key: &str) -> Result<Option<(Section, Value, String)>, EditError> {
    let Some((at, first)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| key_line(line, key))
    else {
        return Ok(None);
    };
    let not_a_map = || EditError::NotABlockMap(key.to_owned());
    let (value, comment) = classify(first, key);
    if matches!(value, Value::Other) {
        return Err(not_a_map());
    }
    let mut end = at + 1;
    let mut child_indent = None;
    for (index, line) in lines.iter().enumerate().skip(at + 1) {
        if is_blank(line) || (is_comment(line) && indent(line) == 0) {
            continue;
        }
        if indent(line) == 0 {
            if child_indent.is_none() && line.starts_with('-') {
                return Err(not_a_map());
            }
            break;
        }
        end = index + 1;
        if has_content(line) && child_indent.is_none() {
            child_name(line).ok_or_else(not_a_map)?;
            child_indent = Some(indent(line));
        }
    }
    Ok(Some((
        Section {
            key: at,
            end,
            child_indent,
        },
        value,
        comment,
    )))
}

fn render_child(name: &str, body: &str, child_indent: usize, unit: usize) -> String {
    let pad = " ".repeat(child_indent);
    let inner = " ".repeat(child_indent + unit);
    let mut out = format!("{pad}{name}:\n");
    for line in body.lines() {
        out.push_str(&inner);
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(super) fn insert_child(
    text: &str,
    key: &str,
    name: &str,
    body: &str,
) -> Result<String, EditError> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((found, value, comment)) = section(&lines, key)? else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(key);
        out.push_str(":\n");
        out.push_str(&render_child(name, body, DEFAULT_INDENT, DEFAULT_INDENT));
        return Ok(out);
    };
    let child_indent = found.child_indent.unwrap_or(DEFAULT_INDENT);
    let child = render_child(name, body, child_indent, child_indent);
    let mut out = String::with_capacity(text.len() + child.len());
    for (at, line) in lines.iter().enumerate() {
        if at == found.key && matches!(value, Value::Empty) {
            out.push_str(key);
            out.push(':');
            out.push_str(&comment);
            out.push('\n');
        } else {
            out.push_str(line);
        }
        if at + 1 == found.end {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&child);
        }
    }
    Ok(out)
}

pub(super) fn remove_children(
    text: &str,
    key: &str,
    mut named: impl FnMut(&str) -> bool,
) -> Result<String, EditError> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((found, value, _)) = section(&lines, key)? else {
        return Ok(text.to_owned());
    };
    let Some(child_indent) = found.child_indent else {
        return Ok(text.to_owned());
    };
    if matches!(value, Value::Empty) {
        return Ok(text.to_owned());
    }
    let mut dropped = BTreeSet::new();
    let mut kept = 0;
    let mut at = found.key + 1;
    while at < found.end {
        let Some(line) = lines
            .get(at)
            .filter(|line| has_content(line) && indent(line) == child_indent)
        else {
            at += 1;
            continue;
        };
        let mut stop = at + 1;
        let mut last = at;
        while let Some(next) = lines
            .get(stop)
            .filter(|next| stop < found.end && (is_blank(next) || indent(next) > child_indent))
        {
            if has_content(next) {
                last = stop;
            }
            stop += 1;
        }
        if child_name(line).is_some_and(|name| named(&name)) {
            dropped.extend(at..=last);
        } else {
            kept += 1;
        }
        at = stop;
    }
    if kept == 0 {
        dropped.insert(found.key);
    }
    Ok(lines
        .iter()
        .enumerate()
        .filter(|(at, _)| !dropped.contains(at))
        .map(|(_, line)| *line)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "command: bash\nargs:\n- -c\n- exec rh mcp\n";

    fn add(text: &str) -> String {
        insert_child(text, "mcp_servers", "rivethub", BODY).unwrap_or_default()
    }

    fn drop_rivethub(text: &str) -> String {
        remove_children(text, "mcp_servers", |name| name == "rivethub").unwrap_or_default()
    }

    #[test]
    fn an_absent_key_is_appended_at_the_end() {
        let original = "model: x\n# keep me\nagent:\n  turns: 3\n";
        let added = add(original);
        assert_eq!(
            added,
            format!(
                "{original}mcp_servers:\n  rivethub:\n    command: bash\n    args:\n    - -c\n    - exec rh mcp\n"
            )
        );
        assert_eq!(drop_rivethub(&added), original);
    }

    #[test]
    fn a_file_without_a_final_newline_gets_one_before_the_key() {
        assert!(add("model: x").starts_with("model: x\nmcp_servers:\n"));
    }

    #[test]
    fn a_new_child_follows_the_indentation_of_its_siblings() {
        for width in [2, 4] {
            let pad = " ".repeat(width);
            let original = format!(
                "a: 1\nmcp_servers:\n{pad}other:\n{pad}{pad}command: x\n{pad}{pad}args: [mcp]\nz: 2\n"
            );
            let added = add(&original);
            let inner = pad.repeat(2);
            let expected = format!(
                "a: 1\nmcp_servers:\n{pad}other:\n{pad}{pad}command: x\n{pad}{pad}args: [mcp]\n{pad}rivethub:\n{inner}command: bash\n{inner}args:\n{inner}- -c\n{inner}- exec rh mcp\nz: 2\n"
            );
            assert_eq!(added, expected, "{width} spaces");
            assert_eq!(drop_rivethub(&added), original, "{width} spaces");
        }
    }

    #[test]
    fn an_empty_flow_map_becomes_a_block_map() {
        let added = add("a: 1\nmcp_servers: {}  # none yet\nz: 2\n");
        assert_eq!(
            added,
            "a: 1\nmcp_servers: # none yet\n  rivethub:\n    command: bash\n    args:\n    - -c\n    - exec rh mcp\nz: 2\n"
        );
    }

    #[test]
    fn a_null_value_becomes_a_block_map() {
        for empty in ["mcp_servers:\n", "mcp_servers: null\n", "mcp_servers: ~\n"] {
            let added = add(&format!("a: 1\n{empty}z: 2\n"));
            assert!(
                added.starts_with("a: 1\nmcp_servers:\n  rivethub:\n    command: bash\n"),
                "{empty}"
            );
            assert!(added.ends_with("z: 2\n"), "{empty}");
        }
    }

    #[test]
    fn other_flow_values_are_refused() {
        for value in ["{a: {command: x}}", "[x]", "\"text\"", "&anchor"] {
            let text = format!("mcp_servers: {value}\n");
            assert!(
                insert_child(&text, "mcp_servers", "rivethub", BODY).is_err(),
                "{value}"
            );
        }
        assert!(insert_child("mcp_servers:\n- x\n", "mcp_servers", "rivethub", BODY).is_err());
    }

    #[test]
    fn comments_and_blank_lines_elsewhere_are_kept_byte_for_byte() {
        let original = "# header\n\nmodel:   x   # trailing\n\nmcp_servers:\n  # about other\n  other:\n    command: x\n\n# footer\nnext: |\n  mcp_servers: not me\n";
        let added = add(original);
        assert!(added.contains("# about other\n  other:\n    command: x\n  rivethub:\n"));
        assert!(added.ends_with("\n\n# footer\nnext: |\n  mcp_servers: not me\n"));
        assert_eq!(drop_rivethub(&added), original);
    }

    #[test]
    fn removing_the_last_child_drops_the_key() {
        let original = "a: 1\nz: 2\n";
        let added = add(original);
        assert_eq!(drop_rivethub(&added), original);
    }

    #[test]
    fn removing_one_child_leaves_its_siblings_intact() {
        let text = "mcp_servers:\n  first:\n    command: a\n  rivethub:\n    command: bash\n    args:\n    - -c\n  last:\n    command: b\n";
        assert_eq!(
            drop_rivethub(text),
            "mcp_servers:\n  first:\n    command: a\n  last:\n    command: b\n"
        );
    }

    #[test]
    fn removing_a_name_that_is_not_there_changes_nothing() {
        let text = "mcp_servers:\n  other:\n    command: a\n";
        assert_eq!(drop_rivethub(text), text);
        assert_eq!(drop_rivethub("a: 1\n"), "a: 1\n");
    }

    #[test]
    fn a_quoted_child_name_is_matched() {
        let text = "mcp_servers:\n  \"rivethub\":\n    command: bash\n  keep:\n    command: a\n";
        assert_eq!(
            drop_rivethub(text),
            "mcp_servers:\n  keep:\n    command: a\n"
        );
    }
}

use thiserror::Error;

const MAX_DEPTH: usize = 64;
const UNIT: &str = "  ";

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum EditError {
    #[error("the document is not plain JSON this editor can place text in")]
    Syntax,
    #[error("{0} is not {1}")]
    Shape(String, &'static str),
}

#[derive(Debug)]
struct Node {
    start: usize,
    end: usize,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Scalar,
}

#[derive(Debug)]
struct Member {
    key: String,
    start: usize,
    value: Node,
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.byte(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), EditError> {
        self.skip_space();
        if self.byte() == Some(wanted) {
            self.pos += 1;
            Ok(())
        } else {
            Err(EditError::Syntax)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, EditError> {
        if depth > MAX_DEPTH {
            return Err(EditError::Syntax);
        }
        self.skip_space();
        let start = self.pos;
        let kind = match self.byte() {
            Some(b'{') => self.object(depth)?,
            Some(b'[') => self.array(depth)?,
            Some(b'"') => {
                self.string()?;
                Kind::Scalar
            }
            Some(_) => {
                self.scalar()?;
                Kind::Scalar
            }
            None => return Err(EditError::Syntax),
        };
        Ok(Node {
            start,
            end: self.pos,
            kind,
        })
    }

    fn string(&mut self) -> Result<(), EditError> {
        self.pos += 1;
        loop {
            match self.byte() {
                None => return Err(EditError::Syntax),
                Some(b'\\') => self.pos += 2,
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn scalar(&mut self) -> Result<(), EditError> {
        let begin = self.pos;
        while !matches!(
            self.byte(),
            None | Some(b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
        ) {
            self.pos += 1;
        }
        if self.pos == begin {
            Err(EditError::Syntax)
        } else {
            Ok(())
        }
    }

    fn object(&mut self, depth: usize) -> Result<Kind, EditError> {
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_space();
        if self.byte() == Some(b'}') {
            self.pos += 1;
            return Ok(Kind::Object(members));
        }
        loop {
            self.skip_space();
            let start = self.pos;
            if self.byte() != Some(b'"') {
                return Err(EditError::Syntax);
            }
            self.string()?;
            let raw = self.text.get(start..self.pos).ok_or(EditError::Syntax)?;
            let key = serde_json::from_str::<String>(raw).map_err(|_| EditError::Syntax)?;
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            members.push(Member { key, start, value });
            self.skip_space();
            match self.byte() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Kind::Object(members));
                }
                _ => return Err(EditError::Syntax),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Kind, EditError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.byte() == Some(b']') {
            self.pos += 1;
            return Ok(Kind::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_space();
            match self.byte() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Kind::Array(items));
                }
                _ => return Err(EditError::Syntax),
            }
        }
    }
}

fn parse(text: &str) -> Result<Node, EditError> {
    let mut parser = Parser { text, pos: 0 };
    let root = parser.value(0)?;
    parser.skip_space();
    if parser.pos == text.len() {
        Ok(root)
    } else {
        Err(EditError::Syntax)
    }
}

impl Node {
    fn member(&self, key: &str) -> Option<&Self> {
        match &self.kind {
            Kind::Object(members) => members
                .iter()
                .find(|member| member.key == key)
                .map(|member| &member.value),
            Kind::Array(_) | Kind::Scalar => None,
        }
    }

    fn at(&self, path: &[&str]) -> Option<&Self> {
        path.iter().try_fold(self, |node, key| node.member(key))
    }

    fn is_empty_container(&self) -> bool {
        match &self.kind {
            Kind::Object(members) => members.is_empty(),
            Kind::Array(items) => items.is_empty(),
            Kind::Scalar => false,
        }
    }
}

fn head(text: &str, at: usize) -> Result<&str, EditError> {
    text.get(..at).ok_or(EditError::Syntax)
}

fn tail(text: &str, at: usize) -> Result<&str, EditError> {
    text.get(at..).ok_or(EditError::Syntax)
}

fn between(text: &str, from: usize, to: usize) -> Result<&str, EditError> {
    text.get(from..to).ok_or(EditError::Syntax)
}

fn leading_space(text: &str, at: usize) -> Result<&str, EditError> {
    let before = head(text, at)?;
    let line = before.rfind('\n').map_or(0, |newline| newline + 1);
    tail(before, line)
}

fn line_indent(text: &str, at: usize) -> Result<&str, EditError> {
    let lead = leading_space(text, at)?;
    let width = lead.len() - lead.trim_start_matches([' ', '\t']).len();
    head(lead, width)
}

fn starts_line(text: &str, at: usize) -> Result<bool, EditError> {
    Ok(leading_space(text, at)?
        .chars()
        .all(|c| c == ' ' || c == '\t'))
}

fn child_indent(
    text: &str,
    container: &Node,
    first_child: Option<usize>,
) -> Result<String, EditError> {
    match first_child {
        Some(start) if starts_line(text, start)? => Ok(line_indent(text, start)?.to_owned()),
        Some(_) | None => Ok(format!("{}{UNIT}", line_indent(text, container.start)?)),
    }
}

fn indented(value: &str, indent: &str) -> String {
    value
        .lines()
        .enumerate()
        .map(|(row, line)| {
            if row == 0 {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn quoted(key: &str) -> Result<String, EditError> {
    serde_json::to_string(key).map_err(|_| EditError::Syntax)
}

fn wrap(key: &str, value: &str) -> Result<String, EditError> {
    Ok(format!(
        "{{\n{UNIT}{}: {}\n}}",
        quoted(key)?,
        indented(value, UNIT)
    ))
}

fn splice(text: &str, from: usize, to: usize, insert: &str) -> Result<String, EditError> {
    Ok(format!("{}{insert}{}", head(text, from)?, tail(text, to)?))
}

fn root_of(text: &str) -> Result<(String, Node), EditError> {
    let text = if text.trim().is_empty() {
        "{}\n".to_owned()
    } else {
        text.to_owned()
    };
    let root = parse(&text)?;
    match root.kind {
        Kind::Object(_) => Ok((text, root)),
        Kind::Array(_) | Kind::Scalar => {
            Err(EditError::Shape("the document".to_owned(), "an object"))
        }
    }
}

pub(super) fn insert_member(
    text: &str,
    path: &[&str],
    key: &str,
    value: &str,
) -> Result<String, EditError> {
    let (text, root) = root_of(text)?;
    let mut current = &root;
    let mut depth = 0;
    for part in path {
        match current.member(part) {
            Some(next) => {
                current = next;
                depth += 1;
            }
            None => break,
        }
    }
    let Kind::Object(members) = &current.kind else {
        return Err(EditError::Shape(path.join("."), "an object"));
    };
    let (name, member_value) = match path.get(depth..).and_then(<[&str]>::split_first) {
        None => (key.to_owned(), value.to_owned()),
        Some((first, rest)) => {
            let mut inner = wrap(key, value)?;
            for part in rest.iter().rev() {
                inner = wrap(part, &inner)?;
            }
            ((*first).to_owned(), inner)
        }
    };
    let indent = child_indent(&text, current, members.first().map(|member| member.start))?;
    let rendered = format!("{}: {}", quoted(&name)?, indented(&member_value, &indent));
    if let Some(existing) = members.iter().find(|member| member.key == name) {
        return splice(
            &text,
            existing.value.start,
            existing.value.end,
            &indented(&member_value, &indent),
        );
    }
    if let Some(last) = members.last() {
        return splice(
            &text,
            last.value.end,
            last.value.end,
            &format!(",\n{indent}{rendered}"),
        );
    }
    let close = line_indent(&text, current.start)?;
    splice(
        &text,
        current.start,
        current.end,
        &format!("{{\n{indent}{rendered}\n{close}}}"),
    )
}

pub(super) fn push_element(text: &str, path: &[&str], value: &str) -> Result<String, EditError> {
    let (mut text, root) = root_of(text)?;
    if root.at(path).is_none() {
        let (last, parent) = path
            .split_last()
            .ok_or_else(|| EditError::Shape("the document".to_owned(), "an array"))?;
        text = insert_member(&text, parent, last, "[]")?;
    }
    let (text, root) = root_of(&text)?;
    let node = root
        .at(path)
        .ok_or_else(|| EditError::Shape(path.join("."), "an array"))?;
    let Kind::Array(items) = &node.kind else {
        return Err(EditError::Shape(path.join("."), "an array"));
    };
    let indent = child_indent(&text, node, items.first().map(|item| item.start))?;
    let rendered = indented(value, &indent);
    if let Some(last) = items.last() {
        return splice(&text, last.end, last.end, &format!(",\n{indent}{rendered}"));
    }
    let close = line_indent(&text, node.start)?;
    splice(
        &text,
        node.start,
        node.end,
        &format!("[\n{indent}{rendered}\n{close}]"),
    )
}

pub(super) fn remove_members(
    text: &str,
    path: &[&str],
    matches: impl Fn(&str) -> bool,
) -> Result<String, EditError> {
    let mut text = text.to_owned();
    loop {
        let root = parse(&text)?;
        let Some(node) = root.at(path) else {
            return Ok(text);
        };
        let Kind::Object(members) = &node.kind else {
            return Err(EditError::Shape(path.join("."), "an object"));
        };
        let Some(index) = members.iter().position(|member| matches(&member.key)) else {
            return Ok(text);
        };
        let spans: Vec<(usize, usize)> = members
            .iter()
            .map(|member| (member.start, member.value.end))
            .collect();
        text = remove_span(&text, node, &spans, index)?;
    }
}

pub(super) fn remove_elements(
    text: &str,
    path: &[&str],
    matches: impl Fn(&str) -> bool,
) -> Result<String, EditError> {
    let mut text = text.to_owned();
    loop {
        let root = parse(&text)?;
        let Some(node) = root.at(path) else {
            return Ok(text);
        };
        let Kind::Array(items) = &node.kind else {
            return Err(EditError::Shape(path.join("."), "an array"));
        };
        let mut found = None;
        for (index, item) in items.iter().enumerate() {
            if matches(between(&text, item.start, item.end)?) {
                found = Some(index);
                break;
            }
        }
        let Some(index) = found else {
            return Ok(text);
        };
        let spans: Vec<(usize, usize)> = items.iter().map(|item| (item.start, item.end)).collect();
        text = remove_span(&text, node, &spans, index)?;
    }
}

fn remove_span(
    text: &str,
    container: &Node,
    spans: &[(usize, usize)],
    index: usize,
) -> Result<String, EditError> {
    let &(start, end) = spans.get(index).ok_or(EditError::Syntax)?;
    if spans.len() == 1 {
        return splice(text, container.start + 1, container.end - 1, "");
    }
    if index == 0 {
        let &(next, _) = spans.get(1).ok_or(EditError::Syntax)?;
        return splice(text, start, next, "");
    }
    let &(_, previous_end) = spans.get(index - 1).ok_or(EditError::Syntax)?;
    splice(text, previous_end, end, "")
}

pub(super) fn prune_empty(text: &str, path: &[&str]) -> Result<String, EditError> {
    let mut text = text.to_owned();
    let mut path = path.to_vec();
    while let Some((last, parent)) = path.split_last() {
        let root = parse(&text)?;
        let empty = root.at(&path).is_some_and(Node::is_empty_container);
        if !empty {
            break;
        }
        text = remove_members(&text, parent, |key| key == *last)?;
        path.pop();
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_lands_after_the_last_one_in_the_files_own_indent() {
        let text = "{\n    \"a\": 1\n}\n";
        let out = insert_member(text, &[], "b", "{\n  \"c\": 2\n}").unwrap();
        assert_eq!(
            out,
            "{\n    \"a\": 1,\n    \"b\": {\n      \"c\": 2\n    }\n}\n"
        );
    }

    #[test]
    fn missing_containers_are_created_on_the_way() {
        let out = insert_member("{}\n", &["x", "y"], "k", "1").unwrap();
        let parsed = parse(&out).unwrap();
        assert!(parsed.at(&["x", "y", "k"]).is_some());
    }

    #[test]
    fn removing_the_only_member_leaves_an_empty_object() {
        let out = remove_members("{\n  \"a\": {\n    \"b\": 1\n  }\n}", &["a"], |key| {
            key == "b"
        })
        .unwrap();
        assert_eq!(out, "{\n  \"a\": {}\n}");
    }

    #[test]
    fn pruning_walks_up_through_emptied_containers() {
        let out = prune_empty("{\"keep\": 1, \"a\": {\"b\": []}}", &["a", "b"]).unwrap();
        assert_eq!(out, "{\"keep\": 1}");
    }

    #[test]
    fn a_document_with_a_comment_is_refused() {
        assert_eq!(
            insert_member("{ // note\n}", &[], "a", "1"),
            Err(EditError::Syntax)
        );
    }
}

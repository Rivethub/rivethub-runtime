use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum EditError {
    #[error("the document is not plain JSON this editor can place text in")]
    Syntax,
    #[error("{0} is not {1}")]
    Shape(String, &'static str),
}

fn parse_root(text: &str) -> Result<Map<String, Value>, EditError> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(text).map_err(|_| EditError::Syntax)? {
        Value::Object(root) => Ok(root),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err(EditError::Shape("the document".to_owned(), "an object"))
        }
    }
}

fn render(root: Map<String, Value>) -> Result<String, EditError> {
    let mut text =
        serde_json::to_string_pretty(&Value::Object(root)).map_err(|_| EditError::Syntax)?;
    text.push('\n');
    Ok(text)
}

fn parse_value(raw: &str) -> Result<Value, EditError> {
    serde_json::from_str(raw).map_err(|_| EditError::Syntax)
}

fn object_at<'a>(
    root: &'a mut Map<String, Value>,
    path: &[&str],
) -> Result<&'a mut Map<String, Value>, EditError> {
    let mut current = root;
    for (depth, part) in path.iter().enumerate() {
        let next = current
            .entry((*part).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        current = next.as_object_mut().ok_or_else(|| {
            EditError::Shape(path.get(..=depth).unwrap_or(path).join("."), "an object")
        })?;
    }
    Ok(current)
}

fn existing<'a>(root: &'a mut Map<String, Value>, path: &[&str]) -> Option<&'a mut Value> {
    let (first, rest) = path.split_first()?;
    rest.iter().try_fold(root.get_mut(*first)?, |value, part| {
        value.as_object_mut()?.get_mut(*part)
    })
}

pub(super) fn insert_member(
    text: &str,
    path: &[&str],
    key: &str,
    value: &str,
) -> Result<String, EditError> {
    let mut root = parse_root(text)?;
    let value = parse_value(value)?;
    object_at(&mut root, path)?.insert(key.to_owned(), value);
    render(root)
}

pub(super) fn push_element(text: &str, path: &[&str], value: &str) -> Result<String, EditError> {
    let mut root = parse_root(text)?;
    let value = parse_value(value)?;
    let (last, parent) = path
        .split_last()
        .ok_or_else(|| EditError::Shape("the document".to_owned(), "an array"))?;
    let slot = object_at(&mut root, parent)?
        .entry((*last).to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    slot.as_array_mut()
        .ok_or_else(|| EditError::Shape(path.join("."), "an array"))?
        .push(value);
    render(root)
}

pub(super) fn remove_members(
    text: &str,
    path: &[&str],
    matches: impl Fn(&str) -> bool,
) -> Result<String, EditError> {
    let mut root = parse_root(text)?;
    let Some(node) = existing(&mut root, path) else {
        return Ok(text.to_owned());
    };
    let members = node
        .as_object_mut()
        .ok_or_else(|| EditError::Shape(path.join("."), "an object"))?;
    let before = members.len();
    members.retain(|name, _| !matches(name));
    if members.len() == before {
        return Ok(text.to_owned());
    }
    render(root)
}

pub(super) fn remove_elements(
    text: &str,
    path: &[&str],
    matches: impl Fn(&str) -> bool,
) -> Result<String, EditError> {
    let mut root = parse_root(text)?;
    let Some(node) = existing(&mut root, path) else {
        return Ok(text.to_owned());
    };
    let items = node
        .as_array_mut()
        .ok_or_else(|| EditError::Shape(path.join("."), "an array"))?;
    let before = items.len();
    items.retain(|item| !matches(&item.to_string()));
    if items.len() == before {
        return Ok(text.to_owned());
    }
    render(root)
}

pub(super) fn prune_empty(text: &str, path: &[&str]) -> Result<String, EditError> {
    let mut root = parse_root(text)?;
    let mut path = path.to_vec();
    let mut changed = false;
    while let Some((last, parent)) = path.split_last() {
        let empty = existing(&mut root, &path).is_some_and(|node| match node {
            Value::Object(members) => members.is_empty(),
            Value::Array(items) => items.is_empty(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
        });
        if !empty {
            break;
        }
        let container = if parent.is_empty() {
            Some(&mut root)
        } else {
            existing(&mut root, parent).and_then(Value::as_object_mut)
        };
        if let Some(container) = container {
            container.shift_remove(*last);
        }
        changed = true;
        path.pop();
    }
    if changed {
        render(root)
    } else {
        Ok(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_lands_after_the_last_one_normalized_to_two_space_pretty() {
        let text = "{\n    \"a\": 1\n}\n";
        let out = insert_member(text, &[], "b", "{\"c\":2}").unwrap();
        assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": 2\n  }\n}\n");
    }

    #[test]
    fn existing_keys_keep_their_order_when_one_is_added_or_replaced() {
        let text = "{\"z\":1,\"a\":2,\"m\":3}";
        let added = insert_member(text, &[], "b", "0").unwrap();
        let keys: Vec<String> = parse_root(&added).unwrap().keys().cloned().collect();
        assert_eq!(keys, ["z", "a", "m", "b"]);
        let replaced = insert_member(&added, &[], "a", "9").unwrap();
        let keys: Vec<String> = parse_root(&replaced).unwrap().keys().cloned().collect();
        assert_eq!(keys, ["z", "a", "m", "b"]);
    }

    #[test]
    fn missing_containers_are_created_on_the_way() {
        let out = insert_member("", &["x", "y"], "k", "1").unwrap();
        assert_eq!(
            out,
            "{\n  \"x\": {\n    \"y\": {\n      \"k\": 1\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn an_element_is_pushed_onto_a_new_or_existing_array() {
        let first = push_element("{}", &["hooks", "Stop"], "{\"a\":1}").unwrap();
        let second = push_element(&first, &["hooks", "Stop"], "{\"b\":2}").unwrap();
        assert_eq!(
            second,
            "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"a\": 1\n      },\n      {\n        \"b\": 2\n      }\n    ]\n  }\n}\n"
        );
    }

    #[test]
    fn removing_the_only_member_leaves_an_empty_object() {
        let out = remove_members("{\n  \"a\": {\n    \"b\": 1\n  }\n}", &["a"], |key| {
            key == "b"
        })
        .unwrap();
        assert_eq!(out, "{\n  \"a\": {}\n}\n");
    }

    #[test]
    fn removing_nothing_returns_the_text_untouched() {
        let text = "{\"a\":   {\"b\": 1}}";
        assert_eq!(
            remove_members(text, &["a"], |key| key == "x").unwrap(),
            text
        );
        assert_eq!(remove_members(text, &["nope"], |_| true).unwrap(), text);
        assert_eq!(prune_empty(text, &["a"]).unwrap(), text);
    }

    #[test]
    fn matching_elements_are_removed_and_the_rest_kept_in_order() {
        let text = "{\"h\":[{\"k\":1},{\"k\":2},{\"k\":3}]}";
        let out = remove_elements(text, &["h"], |raw| raw.contains('2')).unwrap();
        assert_eq!(
            out,
            "{\n  \"h\": [\n    {\n      \"k\": 1\n    },\n    {\n      \"k\": 3\n    }\n  ]\n}\n"
        );
    }

    #[test]
    fn pruning_walks_up_through_emptied_containers() {
        let out = prune_empty("{\"keep\": 1, \"a\": {\"b\": []}}", &["a", "b"]).unwrap();
        assert_eq!(out, "{\n  \"keep\": 1\n}\n");
    }

    #[test]
    fn a_document_with_a_comment_is_refused() {
        assert_eq!(
            insert_member("{ // note\n}", &[], "a", "1"),
            Err(EditError::Syntax)
        );
    }

    #[test]
    fn a_non_object_where_an_object_is_needed_is_refused() {
        assert!(matches!(
            insert_member("{\"a\": 1}", &["a"], "k", "1"),
            Err(EditError::Shape(..))
        ));
        assert!(matches!(
            insert_member("[]", &[], "k", "1"),
            Err(EditError::Shape(..))
        ));
    }
}

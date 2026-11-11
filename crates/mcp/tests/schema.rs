#![cfg(test)]

use mcp::catalog::{optional_non_null, recorded_schema_metadata, search_mode_order};
use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use wire::gateway::memory::{MemoryScope, TagEntryPair};

fn inline_schema<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .subschema_for::<T>()
}

#[test]
fn optional_non_null_preserves_underlying_types_and_constraints() {
    let mut text = inline_schema::<Option<String>>();
    optional_non_null(&mut text);
    assert_eq!(text, inline_schema::<String>());
    let mut scope = inline_schema::<Option<MemoryScope>>();
    optional_non_null(&mut scope);
    assert_eq!(scope, inline_schema::<MemoryScope>());
    let mut entries = inline_schema::<Option<Vec<TagEntryPair>>>();
    optional_non_null(&mut entries);
    assert_eq!(entries, inline_schema::<Vec<TagEntryPair>>());
    let mut constrained = inline_schema::<Option<u32>>();
    constrained.insert("minimum".into(), 1.into());
    constrained.insert("maximum".into(), 50.into());
    optional_non_null(&mut constrained);
    let mut expected = inline_schema::<u32>();
    expected.insert("minimum".into(), 1.into());
    expected.insert("maximum".into(), 50.into());
    assert_eq!(constrained, expected);
}

#[test]
fn recorded_metadata_removes_only_generated_metadata_recursively() {
    let mut schema = inline_schema::<Vec<TagEntryPair>>();
    let expected = schema.clone();
    schema.insert("title".into(), "GeneratedTitle".into());
    schema.insert("default".into(), Vec::<String>::new().into());
    schema.insert("format".into(), "generated-format".into());
    let items = schema
        .get_mut("items")
        .expect("items")
        .as_object_mut()
        .expect("object");
    items.insert("title".into(), "GeneratedItem".into());
    items.insert("default".into(), "generated-default".into());
    items.insert("format".into(), "generated-format".into());
    recorded_schema_metadata(&mut schema);
    assert_eq!(schema, expected);
}

#[test]
fn recorded_search_mode_order_preserves_variants_and_moves_vector_last() {
    let mut schema = inline_schema::<wire::gateway::memory::SearchMode>();
    search_mode_order(&mut schema);
    let variants: Vec<String> =
        serde_json::from_value(schema.get("enum").expect("enum").clone()).expect("strings");
    assert_eq!(variants, ["hybrid", "fts", "trigram", "regex", "vector"]);
    let mut expected = inline_schema::<wire::gateway::memory::SearchMode>();
    expected.insert("enum".into(), serde_json::to_value(variants).expect("enum"));
    assert_eq!(schema, expected);
}

#[test]
fn session_tool_schemas_match_snapshot() {
    let tools = mcp::catalog::session_definitions().expect("session tools");
    let actual = serde_json::to_string_pretty(&tools).expect("tools json");
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session-tools.json");
    let expected = std::fs::read_to_string(&path).expect("session tool snapshot");
    assert_eq!(actual.trim_end(), expected.trim_end());
}

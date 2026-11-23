#![cfg(test)]

use kernel::wiki::builder::extraction_schema;
use serde_json::Value;

#[test]
fn the_extraction_schema_has_an_object_root_with_the_patches_array_inside()
-> Result<(), Box<dyn std::error::Error>> {
    let schema = serde_json::to_value(extraction_schema())?;
    assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
    assert_eq!(
        schema
            .pointer("/properties/patches/type")
            .and_then(Value::as_str),
        Some("array")
    );
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or("missing required array")?;
    assert!(required.contains(&Value::String("patches".to_owned())));
    assert_eq!(
        schema
            .pointer("/$defs/WikiSlug/pattern")
            .and_then(Value::as_str),
        Some("^[a-z0-9]+(-[a-z0-9]+)*$")
    );
    assert_eq!(
        schema
            .pointer("/$defs/WikiHistoryEntry/properties/date/format")
            .and_then(Value::as_str),
        Some("date")
    );
    Ok(())
}

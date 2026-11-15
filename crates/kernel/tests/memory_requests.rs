use std::str::FromStr;

use kernel::memory::Limit;
use kernel::memory::TagLiteral;
use kernel::memory::TagLiteralError;

#[test]
fn tag_literal_parses_key_and_value() {
    let literal = TagLiteral::from_str("project:acme").unwrap();
    assert_eq!(literal.key, "project");
    assert_eq!(literal.value, "acme");
}

#[test]
fn tag_literal_keeps_later_colons_in_the_value() {
    let literal = TagLiteral::from_str("window:12:30").unwrap();
    assert_eq!(literal.key, "window");
    assert_eq!(literal.value, "12:30");
}

#[test]
fn tag_literal_rejects_strings_without_a_literal_shape() {
    assert_eq!(
        TagLiteral::from_str("acme"),
        Err(TagLiteralError::Filter {
            raw: "acme".to_string()
        })
    );
    assert_eq!(
        TagLiteral::from_str("project:"),
        Err(TagLiteralError::Filter {
            raw: "project:".to_string()
        })
    );
    assert_eq!(
        TagLiteral::from_str(":acme"),
        Err(TagLiteralError::Filter {
            raw: ":acme".to_string()
        })
    );
    assert_eq!(
        TagLiteral::from_str(":"),
        Err(TagLiteralError::Filter {
            raw: ":".to_string()
        })
    );
}

#[test]
fn tag_literal_add_path_reports_the_store_error() {
    assert_eq!(
        TagLiteral::parse_add("acme"),
        Err(TagLiteralError::Literal {
            raw: "acme".to_string()
        })
    );
    let literal = TagLiteral::parse_add("project:acme").unwrap();
    assert_eq!(literal, TagLiteral::from_str("project:acme").unwrap());
}

#[test]
fn limit_clamps_to_its_range() {
    assert_eq!(Limit::clamped(None, 20, 1..=50).get(), 20);
    assert_eq!(Limit::clamped(Some(10), 20, 1..=50).get(), 10);
    assert_eq!(Limit::clamped(Some(0), 20, 1..=50).get(), 1);
    assert_eq!(Limit::clamped(Some(500), 20, 1..=50).get(), 50);
    assert_eq!(Limit::clamped(None, 50, 1..=200).get(), 50);
    assert_eq!(Limit::clamped(Some(201), 50, 1..=200).get(), 200);
}

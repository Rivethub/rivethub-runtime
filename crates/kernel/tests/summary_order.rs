#![cfg(test)]

use chrono::{DateTime, Utc};
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use values::JsonObject;
use values::Role;

struct Row {
    id: u8,
    stamp: DateTime<Utc>,
    role: Role,
    metadata: JsonObject,
}

fn row(id: u8, stamp: i64, role: Role, metadata: &str) -> Row {
    Row {
        id,
        stamp: DateTime::from_timestamp(stamp, 0).unwrap(),
        role,
        metadata: serde_json::from_str(metadata).unwrap(),
    }
}

fn ordered(mut rows: Vec<Row>) -> Vec<u8> {
    rows.sort_by_key(|row| (row.stamp, row.id));
    order_messages(rows, MemoryBrowseOrder::Asc, |row| {
        MessagePosition::new(row.stamp, row.role, Some(&row.metadata))
    })
    .into_iter()
    .map(|row| row.id)
    .collect()
}

#[test]
fn merges_timestamp_fallback_with_line_precedence_and_correlated_turn() {
    let rows = vec![
        row(
            1,
            30,
            Role::Assistant,
            r#"{"session_jsonl_line": 0, "uuid": "turn-a"}"#,
        ),
        row(2, 10, Role::User, r#"{"session_jsonl_line": 2}"#),
        row(3, 90, Role::Tool, r#"{"uuid": "turn-a"}"#),
        row(4, 20, Role::User, r#"{"session_jsonl_line": -1}"#),
        row(5, 40, Role::User, r#"{"session_jsonl_line": "3"}"#),
    ];
    assert_eq!(ordered(rows), [4, 1, 3, 2, 5]);
}

#[test]
fn compares_only_stream_heads_across_paths() {
    let rows = vec![
        row(
            1,
            30,
            Role::User,
            r#"{"session_jsonl_path": "a", "session_jsonl_line": 1}"#,
        ),
        row(
            2,
            10,
            Role::User,
            r#"{"session_jsonl_path": "a", "session_jsonl_line": 2}"#,
        ),
        row(
            3,
            15,
            Role::User,
            r#"{"session_jsonl_path": "b", "session_jsonl_line": 8}"#,
        ),
        row(
            4,
            40,
            Role::User,
            r#"{"session_jsonl_path": "b", "session_jsonl_line": 9}"#,
        ),
    ];
    assert_eq!(ordered(rows), [3, 1, 2, 4]);
}

#[test]
fn same_line_prefers_subordinal_then_assistant_and_keeps_call_cluster() {
    let rows = vec![
        row(
            1,
            30,
            Role::Assistant,
            r#"{"session_jsonl_line": 2, "ordinal": 1}"#,
        ),
        row(
            2,
            10,
            Role::Tool,
            r#"{"session_jsonl_line": 2, "ordinal": 1, "event_id": "s:tool:x"}"#,
        ),
        row(3, 90, Role::Tool, r#"{"event_id": "s:tool:x"}"#),
        row(
            4,
            40,
            Role::Tool,
            r#"{"session_jsonl_line": 2, "ordinal": 0}"#,
        ),
        row(5, 50, Role::Tool, r#"{"event_id": "other:tool:x"}"#),
    ];
    assert_eq!(ordered(rows), [4, 1, 2, 3, 5]);
}

#[test]
fn ambiguous_turn_uses_timestamp_unless_the_path_identifies_it() {
    let rows = vec![
        row(
            1,
            30,
            Role::Assistant,
            r#"{"session_jsonl_path": "a", "session_jsonl_line": 1, "uuid": "turn-a"}"#,
        ),
        row(
            2,
            40,
            Role::Assistant,
            r#"{"session_jsonl_path": "b", "session_jsonl_line": 1, "uuid": "turn-a"}"#,
        ),
        row(3, 20, Role::Tool, r#"{"assistant_uuid": "turn-a"}"#),
        row(
            4,
            90,
            Role::Tool,
            r#"{"session_jsonl_path": "a", "assistant_uuid": "turn-a"}"#,
        ),
    ];
    assert_eq!(ordered(rows), [3, 1, 4, 2]);
}

#[test]
fn all_unlined_preserves_legacy_order_for_every_small_timestamp_assignment() {
    for assignment in 0..81 {
        let rows: Vec<_> = (0_u8..4)
            .rev()
            .map(|id| {
                let stamp = i64::from(assignment / 3_i32.pow(u32::from(id)) % 3);
                row(id, stamp, Role::Tool, r#"{"event_id": "s:tool:x"}"#)
            })
            .collect();
        let mut legacy: Vec<_> = rows.iter().map(|row| (row.stamp, row.id)).collect();
        legacy.sort_unstable();
        assert_eq!(
            ordered(rows),
            legacy.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        );
    }
}

#[test]
fn out_of_range_cursor_timestamps_are_errors() {
    use kernel::memory::MessageCursor;
    use uuid::Uuid;

    for micros in [i64::MIN, i64::MAX] {
        let encoded = format!("{micros}.{}", Uuid::nil());
        let error = MessageCursor::decode(&encoded).expect_err("out-of-range cursor time");
        assert_eq!(
            error.to_string(),
            format!("cursor {encoded:?} is not one this server issued")
        );
    }
}

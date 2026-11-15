use capture::event_id::OccurrenceKind;
use capture::event_id::ToolInput;

const TOOL_DIGEST: &str = "146febb19d952f1e7867c264405749f516bbd1a018c358ce43f86fa330080495";
const ASSISTANT_DIGEST: &str = "82c65e8f44c13f18ad14a0fb2031efafe4512258601b707cdb65d2d6c594fa54";

fn arguments(text: &str) -> Result<ToolInput, serde_json::Error> {
    ToolInput::parse(text)
}

#[test]
fn tool_occurrence_digest_matches_the_recorded_id() -> Result<(), serde_json::Error> {
    let input = arguments(r#"{"file_path":"/work/demo/a.txt"}"#)?;
    assert_eq!(
        capture::event_id::occurrence_digest(
            OccurrenceKind::Tool,
            "[tool call] Read",
            "Read",
            Some(&input)
        ),
        TOOL_DIGEST
    );
    Ok(())
}

#[test]
fn assistant_occurrence_digest_matches_the_recorded_id() {
    assert_eq!(
        capture::event_id::occurrence_digest(OccurrenceKind::Assistant, "no uuid", "", None),
        ASSISTANT_DIGEST
    );
}

#[test]
fn occurrence_ids_carry_the_occurrence_index() -> Result<(), serde_json::Error> {
    let input = arguments(r#"{"file_path":"/work/demo/a.txt"}"#)?;
    let digest = capture::event_id::occurrence_digest(
        OccurrenceKind::Tool,
        "[tool call] Read",
        "Read",
        Some(&input),
    );
    assert_eq!(
        capture::event_id::occurrence_event_id("claude-code:s", &digest, 2),
        format!("claude-code:s:occ:{digest}:2")
    );
    Ok(())
}

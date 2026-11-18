use testkit::Json;
use testkit::json_eq;

#[test]
fn batch_session_dates_accept_relaxed_offsets_and_reject_offset_seconds()
-> Result<(), Box<dyn std::error::Error>> {
    let raw = r#"{"session_key":"codex:session","agent":"codex","messages":[],"created_at":" 2026-01-01T01:02:03.123456789012 +0100","updated_at":"2026-01-01T01:02:03Z"}"#;
    let batch = capture::batch::parse_batch(raw)?;
    let json = serde_json::to_value(batch)?;
    assert_eq!(
        json.get("created_at").and_then(serde_json::Value::as_str),
        Some("2026-01-01T00:02:03.123Z")
    );
    assert_eq!(
        json.get("updated_at").and_then(serde_json::Value::as_str),
        Some("2026-01-01T01:02:03.000Z")
    );
    assert!(capture::batch::parse_batch(&raw.replace(" +0100", "+01:00:00")).is_err());
    Ok(())
}

#[test]
fn batch_validation_matches_recorded_outcomes() -> Result<(), Box<dyn std::error::Error>> {
    let path = testkit::recording("capture/capture-zod/capture-batch.json");
    let text = std::fs::read_to_string(&path)?;
    let recording: Json = serde_json::from_str(&text)?;
    let cases = recording
        .get("cases")
        .and_then(testkit::Json::as_object)
        .ok_or("cases missing")?;
    let mut failures = Vec::new();
    for (name, case) in cases {
        let raw = serde_json::to_string(&case["input"])?;
        let expected_ok = case
            .get("ok")
            .and_then(testkit::Json::as_bool)
            .ok_or("case without ok")?;
        match capture::batch::parse_batch(&raw) {
            Ok(batch) => {
                if !expected_ok {
                    failures.push(format!("{name}: accepted, recorded rejection"));
                    continue;
                }
                let Some(expected) = case.get("parsed") else {
                    continue;
                };
                let actual = serde_json::to_value(&batch)?;
                if !json_eq(expected, &actual) {
                    failures.push(format!(
                        "{name}: parsed batch mismatch\n    recorded: {expected}\n    actual:   {actual}"
                    ));
                }
            }
            Err(error) => {
                if expected_ok {
                    failures.push(format!("{name}: rejected: {error}"));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} validation failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        )
        .into())
    }
}

use serde_json::json;
use wire::capture::{CaptureMessage, SanitizeStamp};

#[test]
fn timestamp_precision_survives_wire_serialization() -> Result<(), Box<dyn std::error::Error>> {
    for (raw, automatic, millis) in [
        (
            "2025-12-31T23:59:59Z",
            "2025-12-31T23:59:59Z",
            "2025-12-31T23:59:59.000Z",
        ),
        (
            "2026-01-01t01:00:00.5+01:00",
            "2026-01-01T00:00:00.500Z",
            "2026-01-01T00:00:00.500Z",
        ),
        (
            "1969-12-31 23:59:59.123456z",
            "1969-12-31T23:59:59.123456Z",
            "1969-12-31T23:59:59.123Z",
        ),
        (
            "2026-01-01T00:00:00.123456789Z",
            "2026-01-01T00:00:00.123456789Z",
            "2026-01-01T00:00:00.123Z",
        ),
    ] {
        let stamp: SanitizeStamp = serde_json::from_value(json!({
            "at": raw, "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
        }))?;
        let capture: CaptureMessage = serde_json::from_value(json!({
            "event_id": "message", "role": "user", "content": "body", "created_at": raw
        }))?;
        assert_eq!(serde_json::to_value(stamp)?["at"], automatic);
        assert_eq!(serde_json::to_value(capture)?["created_at"], millis);
    }
    Ok(())
}

#[test]
fn wire_instants_require_complete_dates_times_and_offsets() {
    for raw in [
        "2026-01-01",
        "2026-01-01T00:00:00",
        "2026-01-01T00:00Z",
        "2026-01-01T00:00:00Z[UTC]",
        "2026-02-29T00:00:00Z",
    ] {
        assert!(
            serde_json::from_value::<SanitizeStamp>(json!({
                "at": raw, "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
            }))
            .is_err(),
            "{raw}"
        );
        assert!(
            serde_json::from_value::<CaptureMessage>(json!({
                "event_id": "message", "role": "user", "content": "body", "created_at": raw
            }))
            .is_err(),
            "{raw}"
        );
    }
}

#[test]
fn default_wire_parser_accepts_an_offset_without_a_colon() -> Result<(), Box<dyn std::error::Error>>
{
    let raw = "2026-01-01T00:00:00+0100";
    let stamp: SanitizeStamp = serde_json::from_value(json!({
        "at": raw, "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
    }))?;
    assert_eq!(serde_json::to_value(stamp)?["at"], "2025-12-31T23:00:00Z");
    assert!(
        serde_json::from_value::<CaptureMessage>(json!({
            "event_id": "message", "role": "user", "content": "body", "created_at": raw
        }))
        .is_err()
    );
    Ok(())
}

#[test]
fn leap_seconds_survive_capture_and_canonical_wire_output() -> Result<(), Box<dyn std::error::Error>>
{
    let stamp: SanitizeStamp = serde_json::from_value(json!({
        "at": "2016-12-31T23:59:60.5Z", "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
    }))?;
    let capture: CaptureMessage = serde_json::from_value(json!({
        "event_id": "message", "role": "user", "content": "body", "created_at": "2016-12-31T23:59:60.5Z"
    }))?;
    assert_eq!(
        serde_json::to_value(stamp)?["at"],
        "2016-12-31T23:59:60.500Z"
    );
    assert_eq!(
        serde_json::to_value(capture)?["created_at"],
        "2016-12-31T23:59:60.500Z"
    );
    Ok(())
}

#[test]
fn default_wire_timestamps_keep_expanded_years() -> Result<(), Box<dyn std::error::Error>> {
    let stamp: SanitizeStamp = serde_json::from_value(json!({
        "at": "+10000-01-01T00:00:00Z", "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
    }))?;
    assert_eq!(serde_json::to_value(stamp)?["at"], "+10000-01-01T00:00:00Z");
    Ok(())
}

#[test]
fn plugin_timestamps_round_to_a_signed_millisecond() -> Result<(), Box<dyn std::error::Error>> {
    use wire::plugin::process::PluginEventDelivery;

    let mut event: PluginEventDelivery = serde_json::from_value(json!({
        "seq": 1, "at": -1, "missed": 0, "event": { "type": "settings.changed", "values": {} }
    }))?;
    assert_eq!(serde_json::to_value(&event)?["at"], -1);
    for (raw, expected) in [
        ("1969-12-31T23:59:59.999500000Z", -1),
        ("1969-12-31T23:59:59.999500001Z", 0),
        ("1970-01-01T00:00:00.000500000Z", 1),
    ] {
        event.at = raw.parse()?;
        assert_eq!(serde_json::to_value(&event)?["at"], expected, "{raw}");
    }
    Ok(())
}

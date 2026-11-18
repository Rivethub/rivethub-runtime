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
        assert_eq!(
            serde_json::to_value(stamp)?
                .get("at")
                .and_then(serde_json::Value::as_str),
            Some(automatic)
        );
        assert_eq!(
            serde_json::to_value(capture)?
                .get("created_at")
                .and_then(serde_json::Value::as_str),
            Some(millis)
        );
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
    assert_eq!(
        serde_json::to_value(stamp)?
            .get("at")
            .and_then(serde_json::Value::as_str),
        Some("2025-12-31T23:00:00Z")
    );
    assert!(
        serde_json::from_value::<CaptureMessage>(json!({
            "event_id": "message", "role": "user", "content": "body", "created_at": raw
        }))
        .is_err()
    );
    Ok(())
}

#[test]
fn leap_seconds_normalize_to_the_preceding_second() -> Result<(), Box<dyn std::error::Error>> {
    let stamp: SanitizeStamp = serde_json::from_value(json!({
        "at": "2016-12-31T23:59:60.5Z", "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
    }))?;
    let capture: CaptureMessage = serde_json::from_value(json!({
        "event_id": "message", "role": "user", "content": "body", "created_at": "2016-12-31T23:59:60.5Z"
    }))?;
    assert_eq!(
        serde_json::to_value(stamp)?
            .get("at")
            .and_then(serde_json::Value::as_str),
        Some("2016-12-31T23:59:59.500Z")
    );
    assert_eq!(
        serde_json::to_value(capture)?
            .get("created_at")
            .and_then(serde_json::Value::as_str),
        Some("2016-12-31T23:59:59.500Z")
    );
    Ok(())
}

#[test]
fn default_wire_timestamps_reject_a_year_outside_the_supported_range() {
    assert!(serde_json::from_value::<SanitizeStamp>(json!({
        "at": "+10000-01-01T00:00:00Z", "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
    })).is_err());
}

#[test]
fn plugin_timestamps_round_to_a_signed_millisecond() -> Result<(), Box<dyn std::error::Error>> {
    use wire::plugin::process::PluginEventDelivery;

    let mut event: PluginEventDelivery = serde_json::from_value(json!({
        "seq": 1, "at": -1, "missed": 0, "event": { "type": "settings.changed", "values": {} }
    }))?;
    assert_eq!(
        serde_json::to_value(&event)?
            .get("at")
            .and_then(serde_json::Value::as_i64),
        Some(-1)
    );
    for (raw, expected) in [
        ("1969-12-31T23:59:59.999500000Z", -1),
        ("1969-12-31T23:59:59.999500001Z", 0),
        ("1970-01-01T00:00:00.000500000Z", 1),
    ] {
        event.at = raw.parse()?;
        assert_eq!(
            serde_json::to_value(&event)?
                .get("at")
                .and_then(serde_json::Value::as_i64),
            Some(expected),
            "{raw}"
        );
    }
    Ok(())
}

#[test]
fn legacy_wire_spelling_and_precision() -> Result<(), Box<dyn std::error::Error>> {
    for (raw, default, capture) in [
        (
            " 2026- 1- 1t 01 : 02 : 03.123456789012 +01::00 ",
            Some("2026-01-01T00:02:03.123456789Z"),
            None,
        ),
        ("2026-01-01T01:02:03+ 0100", None, None),
        ("2026-01-01T01:02:03+24:00", None, None),
        ("2026-01-01T01:02:03-25:59", None, None),
        (
            "2026-01-01 01:02:03.123456789012z",
            Some("2026-01-01T01:02:03.123456789Z"),
            Some("2026-01-01T01:02:03.123Z"),
        ),
        (
            "2026-01-01T01:02:03−01:00",
            Some("2026-01-01T02:02:03Z"),
            Some("2026-01-01T02:02:03.000Z"),
        ),
        (
            "-0001-01-01T01:02:03.5Z",
            Some("-0001-01-01T01:02:03.500Z"),
            None,
        ),
    ] {
        let stamp = serde_json::from_value::<SanitizeStamp>(json!({
            "at": raw, "policy_sha256": "hash", "passes": [], "unresolved_flags": 0
        }));
        let message = serde_json::from_value::<CaptureMessage>(json!({
            "event_id": "message", "role": "user", "content": "body", "created_at": raw
        }));
        assert_eq!(stamp.is_ok(), default.is_some(), "{raw}");
        assert_eq!(message.is_ok(), capture.is_some(), "{raw}");
        if let Ok(stamp) = stamp {
            assert_eq!(
                serde_json::to_value(stamp)?
                    .get("at")
                    .and_then(serde_json::Value::as_str),
                default,
                "{raw}"
            );
        }
        if let Ok(message) = message {
            assert_eq!(
                serde_json::to_value(message)?
                    .get("created_at")
                    .and_then(serde_json::Value::as_str),
                capture,
                "{raw}"
            );
        }
    }
    Ok(())
}

#[test]
fn negative_year_format_spelling() -> Result<(), Box<dyn std::error::Error>> {
    use wire::time::LegacyStrftime as _;

    let at = wire::time::parse_wire_timestamp("-0001-01-01T01:02:03.5Z")?;
    assert_eq!(
        at.legacy_strftime("%Y%m%dT%H%M%S%3fZ"),
        "-00010101T010203500Z"
    );
    assert_eq!(at.legacy_strftime("%Y-%m-%d"), "-0001-01-01");
    Ok(())
}

#[test]
fn civil_dates_keep_legacy_year_width_and_serde_whitespace()
-> Result<(), Box<dyn std::error::Error>> {
    use serde_with::{As, DeserializeAs as _};
    use wire::time::{CivilDate, LegacyStrftime as _, parse_civil_date};

    let signed = parse_civil_date("+02026-1-1")?;
    assert_eq!(signed.legacy_strftime("%Y-%m-%d"), "2026-01-01");
    assert!(parse_civil_date(" 2026 - 1 - 1 ").is_err());
    let raw = r#"" 2026 - 1 - 1 ""#;
    let date = CivilDate::deserialize_as(&mut serde_json::Deserializer::from_str(raw))?;
    assert_eq!(
        As::<CivilDate>::serialize(&date, serde_json::value::Serializer)?,
        "2026-01-01"
    );
    Ok(())
}

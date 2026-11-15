use std::str::FromStr as _;

use harness_acp::IdentityError;
use harness_acp::NativeSession;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

const UUID: &str = "00000000-0000-4000-8000-000000000000";

fn canonical(harness: HarnessId, native: &str) -> String {
    format!("{harness}:{native}")
}

#[test]
fn uuid_harnesses_accept_hyphenated_uuids() {
    for harness in [
        HarnessId::ClaudeCode,
        HarnessId::Omp,
        HarnessId::Dsh,
        HarnessId::Cursor,
        HarnessId::Cowork,
        HarnessId::Codex,
    ] {
        let parsed = NativeSession::parse(harness, UUID).expect("uuid accepted");
        assert_eq!(parsed.harness(), harness);
        assert_eq!(parsed.native(), UUID);
        assert_eq!(
            parsed.canonical(),
            SessionId::from_str(&canonical(harness, UUID)).expect("canonical parses")
        );
    }
}

#[test]
fn uuid_harnesses_reject_non_uuids_and_non_canonical_forms() {
    for native in [
        "",
        "not-a-uuid",
        "00000000000040008000000000000000",
        "00000000-0000-4000-8000-00000000000g",
        "urn:uuid:00000000-0000-4000-8000-000000000000",
        "{00000000-0000-4000-8000-000000000000}",
        " 00000000-0000-4000-8000-000000000000",
    ] {
        let parsed = NativeSession::parse(HarnessId::ClaudeCode, native);
        assert!(
            matches!(parsed, Err(IdentityError::Shape { .. })),
            "claude-code must reject {native:?}"
        );
    }
}

#[test]
fn grok_build_requires_a_version_7_uuid() {
    let v4 = "00000000-0000-4000-8000-000000000000";
    let parsed = NativeSession::parse(HarnessId::GrokBuild, v4);
    assert!(
        matches!(parsed, Err(IdentityError::UuidVersion { .. })),
        "grok-build requires uuidv7, rejected {v4:?}"
    );
    let v7 = "0198c0de-7a5b-7000-8000-000000000001";
    assert!(NativeSession::parse(HarnessId::GrokBuild, v7).is_ok());
    assert!(NativeSession::parse(HarnessId::ClaudeCode, v7).is_ok());
}

#[test]
fn hermes_accepts_stamped_ids_and_rejects_unknown_prefixed_ones() {
    let native = "20260102_030405_abc123";
    let parsed = NativeSession::parse(HarnessId::Hermes, native).expect("hermes shape");
    assert_eq!(
        parsed.canonical().as_str(),
        &canonical(HarnessId::Hermes, native)
    );
    for native in [
        "unknown-20260102_030405_abc123",
        "20260102_030405_abc12",
        "20260102_03040_5abc123",
        "20260132_030405_abc123",
        "20260102_036005_abc123",
        "20260102-030405-abc123",
        "20260102_030405",
    ] {
        let parsed = NativeSession::parse(HarnessId::Hermes, native);
        assert!(
            matches!(
                parsed,
                Err(IdentityError::Shape { .. } | IdentityError::Timestamp { .. })
            ),
            "hermes must reject {native:?}"
        );
    }
}

#[test]
fn hermes_accepts_acp_uuid_sessions() {
    let native = "a73e7e03-185c-4a17-88db-d5ab15bff69d";
    let parsed = NativeSession::parse(HarnessId::Hermes, native).expect("hermes ACP uuid");
    assert_eq!(
        parsed.canonical().as_str(),
        &canonical(HarnessId::Hermes, native)
    );
    for native in [
        "A73E7E03-185C-4A17-88DB-D5AB15BFF69D",
        "a73e7e03185c4a1788dbd5ab15bff69d",
    ] {
        assert!(
            matches!(
                NativeSession::parse(HarnessId::Hermes, native),
                Err(IdentityError::Shape { .. })
            ),
            "hermes must reject {native:?}"
        );
    }
}

#[test]
fn kimi_accepts_session_prefixed_uuids_only() {
    let native = "session_00000000-0000-4000-8000-000000000000";
    let parsed = NativeSession::parse(HarnessId::KimiCode, native).expect("kimi shape");
    assert_eq!(
        parsed.canonical().as_str(),
        &canonical(HarnessId::KimiCode, native)
    );
    for native in [
        "00000000-0000-4000-8000-000000000000",
        "session_not-a-uuid",
        "session_",
        "20260102_030405_abc123",
    ] {
        let parsed = NativeSession::parse(HarnessId::KimiCode, native);
        assert!(
            matches!(parsed, Err(IdentityError::Shape { .. })),
            "kimi-code must reject {native:?}"
        );
    }
}

#[test]
fn opencode_accepts_ses_prefixed_ids_with_twenty_alphanumerics() {
    let native = "ses_0123456789abcdef0123";
    let parsed = NativeSession::parse(HarnessId::Opencode, native).expect("opencode shape");
    assert_eq!(
        parsed.canonical().as_str(),
        &canonical(HarnessId::Opencode, native)
    );
    let longer = "ses_0123456789abcdef0123456789abcdef";
    assert!(NativeSession::parse(HarnessId::Opencode, longer).is_ok());
    for native in [
        "ses_0123456789abcdef012",
        "0123456789abcdef0123",
        "ses_0123456789abcdefgh0123!",
        "ses_",
    ] {
        let parsed = NativeSession::parse(HarnessId::Opencode, native);
        assert!(
            matches!(parsed, Err(IdentityError::Shape { .. })),
            "opencode must reject {native:?}"
        );
    }
}

#[test]
fn canonical_ids_round_trip_through_the_native_parser() {
    for (harness, native) in [
        (HarnessId::ClaudeCode, UUID),
        (HarnessId::GrokBuild, "0198c0de-7a5b-7000-8000-000000000001"),
        (HarnessId::Hermes, "20260102_030405_abc123"),
        (
            HarnessId::KimiCode,
            "session_00000000-0000-4000-8000-000000000000",
        ),
        (HarnessId::Opencode, "ses_0123456789abcdef0123"),
        (HarnessId::Codex, UUID),
    ] {
        let session = SessionId::from_str(&canonical(harness, native)).expect("canonical parses");
        let parsed = harness_acp::identity::from_canonical(&session).expect("round trip");
        assert_eq!(parsed.harness(), harness);
        assert_eq!(parsed.native(), native);
        assert_eq!(parsed.canonical(), session);
    }
}

#[test]
fn a_canonical_id_with_a_malformed_native_part_is_rejected() {
    let session = SessionId::from_str("claude-code:not-a-uuid").expect("canonical parses");
    let parsed = harness_acp::identity::from_canonical(&session);
    assert!(matches!(parsed, Err(IdentityError::Shape { .. })));
}

#[test]
fn malformed_hermes_timestamps_preserve_the_parse_error() {
    use std::error::Error as _;
    let error = NativeSession::parse(HarnessId::Hermes, "20260230_120000_abcdef")
        .expect_err("invalid date");
    assert!(matches!(error, IdentityError::Timestamp { .. }));
    assert!(error.source().is_some());
}

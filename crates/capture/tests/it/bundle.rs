use capture::bundle::BundleError;
use chrono::DateTime;
use chrono::Utc;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(serde::Serialize)]
struct FirstBundle<'a> {
    version: u32,
    sessions: &'a [CaptureBatchRequest],
}

fn first_bundle(sessions: &[CaptureBatchRequest]) -> Result<String, serde_json::Error> {
    serde_json::to_string(&FirstBundle {
        version: 1,
        sessions,
    })
}

#[derive(serde::Deserialize)]
struct Stamped {
    manifest: StampedManifest,
}

#[derive(serde::Deserialize)]
struct StampedManifest {
    sanitization: StampState,
}

#[derive(serde::Deserialize)]
struct StampState {
    state: String,
}

fn instant(text: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

fn message(event_id: &str, content: &str, at: Option<DateTime<Utc>>) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: at,
    }
}

fn batch(session_key: &str, messages: Vec<CaptureMessage>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: "rivet".to_owned(),
        channel: Some("claude-code".to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    }
}

fn sealed() -> Result<wire::capture::SessionBundle, Box<dyn std::error::Error>> {
    sealed_with(None)
}

fn sealed_with(
    settings: Option<JsonObject>,
) -> Result<wire::capture::SessionBundle, Box<dyn std::error::Error>> {
    let at = instant("2026-10-14T09:00:00Z")?;
    let session = BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "0195f0c2-0000-7000-8000-000000000001".to_owned(),
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            created_at: Some(at),
            updated_at: Some(at),
        },
        batch: CaptureBatchRequest {
            settings,
            ..batch(
                "claude-code:one",
                vec![message("evt-1", "ship the bundle", Some(at))],
            )
        },
    };
    let author = BundleAuthor {
        user: "phil".to_owned(),
        node: "desk".to_owned(),
    };
    Ok(capture::bundle::seal(author, at, Sanitization::Raw, vec![session])?.bundle)
}

#[test]
fn a_sealed_bundle_round_trips_through_its_file_text() -> TestResult {
    let bundle = sealed()?;
    let text = serde_json::to_string_pretty(&bundle)?;
    assert_eq!(capture::bundle::parse(&text)?, bundle);
    assert_eq!(bundle.manifest.session_count, 1);
    assert_eq!(bundle.manifest.sanitization, Sanitization::Raw);
    let stamped: Stamped = serde_json::from_str(&text)?;
    assert_eq!(stamped.manifest.sanitization.state, "raw");
    Ok(())
}

#[test]
fn the_sealed_text_is_the_canonical_json_of_the_bundle_it_parses_to() -> TestResult {
    let bundle = sealed()?;
    let sealed = capture::bundle::seal(
        bundle.manifest.exported_by.clone(),
        bundle.manifest.exported_at,
        Sanitization::Raw,
        bundle.sessions.clone(),
    )?;
    let parsed = capture::bundle::parse(std::str::from_utf8(&sealed.text)?)?;
    assert_eq!(parsed, sealed.bundle);
    assert_eq!(parsed, bundle);
    assert_eq!(sealed.text, wire::json::canonical_json(&parsed)?);
    Ok(())
}

#[test]
fn a_v1_bundle_parses_as_v2_with_inferred_origin() -> TestResult {
    let opened = instant("2026-10-14T09:00:00Z")?;
    let closed = instant("2026-10-14T11:30:00Z")?;
    let mut dated = batch(
        "claude-code:one",
        vec![message("evt-1", "ship the bundle", Some(opened))],
    );
    dated.created_at = Some(opened);
    dated.updated_at = Some(closed);
    let from_messages = batch(
        "codex:two",
        vec![
            message("evt-2", "first", Some(opened)),
            message("evt-3", "last", Some(closed)),
        ],
    );

    let bundle = capture::bundle::parse(&first_bundle(&[dated.clone(), from_messages])?)?;

    assert_eq!(bundle.version, 2);
    assert_eq!(bundle.manifest.session_count, 2);
    assert_eq!(bundle.manifest.exported_by.user, "unknown");
    assert_eq!(bundle.manifest.exported_at, closed);
    assert_eq!(
        bundle.manifest.bundle_sha256,
        capture::bundle::bundle_sha256(&bundle)?
    );
    let origins: Vec<_> = bundle
        .sessions
        .iter()
        .map(|session| {
            (
                session.origin.session_key.as_str(),
                session.origin.author.as_str(),
                session.origin.created_at,
                session.origin.updated_at,
            )
        })
        .collect();
    assert_eq!(
        origins,
        [
            ("claude-code:one", "unknown", Some(opened), Some(closed)),
            ("codex:two", "unknown", Some(opened), Some(closed)),
        ]
    );
    let first = bundle.sessions.first().ok_or("first session")?;
    assert_eq!(first.batch, dated);
    Ok(())
}

#[test]
fn a_v1_session_with_no_dates_parses_with_unknown_dates() -> TestResult {
    let undated = batch("undated", vec![message("evt-1", "hi", None)]);
    let bundle = capture::bundle::parse(&first_bundle(std::slice::from_ref(&undated))?)?;
    let session = bundle.sessions.first().ok_or("undated session")?;
    assert_eq!(session.batch, undated);
    assert_eq!(
        (session.origin.created_at, session.origin.updated_at),
        (None, None)
    );
    assert_eq!(bundle.manifest.exported_at, DateTime::<Utc>::UNIX_EPOCH);
    Ok(())
}

#[test]
fn a_bundle_with_a_wrong_digest_is_rejected() -> TestResult {
    let text = serde_json::to_string_pretty(&sealed()?)?;
    let tampered = text.replace("ship the bundle", "ship the bundlf");
    assert_ne!(tampered, text);
    let error = capture::bundle::parse(&tampered).err();
    assert!(
        matches!(error, Some(BundleError::Digest { .. })),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_with_an_edited_manifest_is_rejected() -> TestResult {
    let text = serde_json::to_string(&sealed()?)?;
    let edits = [
        (
            r#""user":"phil","node":"desk"}"#,
            r#""user":"mallory","node":"desk"}"#,
        ),
        (
            r#""sanitization":{"state":"raw"}"#,
            r#""sanitization":{"state":"sanitized","at":"2026-11-08T00:00:00Z","policy_sha256":"00","passes":["all"],"unresolved_flags":0}"#,
        ),
        (
            r#""exported_at":"2026-10-14T09:00:00Z""#,
            r#""exported_at":"2026-10-15T09:00:00Z""#,
        ),
    ];
    for (from, to) in edits {
        let forged = text.replacen(from, to, 1);
        assert_ne!(forged, text, "{from} must occur in the sealed text");
        let error = capture::bundle::parse(&forged).err();
        assert!(
            matches!(error, Some(BundleError::Digest { .. })),
            "{from}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn the_digest_ignores_key_order_and_whitespace() -> TestResult {
    let ordered = sealed_with(Some(serde_json::from_str(r#"{"a":1,"b":{"c":2,"d":3}}"#)?))?;
    let reversed = sealed_with(Some(serde_json::from_str(r#"{"b":{"d":3,"c":2},"a":1}"#)?))?;
    assert_eq!(
        ordered.manifest.bundle_sha256,
        reversed.manifest.bundle_sha256
    );
    let compact = serde_json::to_string(&reversed)?;
    let pretty = serde_json::to_string_pretty(&ordered)?.replace(
        &ordered.manifest.bundle_sha256,
        &reversed.manifest.bundle_sha256,
    );
    assert_eq!(capture::bundle::parse(&compact)?, reversed);
    assert_eq!(capture::bundle::parse(&pretty)?, ordered);
    Ok(())
}

#[test]
fn a_miscounted_or_unknown_version_bundle_is_refused() -> TestResult {
    let text = serde_json::to_string(&sealed()?)?;
    let miscounted = text.replace(r#""session_count":1"#, r#""session_count":2"#);
    assert_ne!(miscounted, text);
    let error = capture::bundle::parse(&miscounted).err();
    assert!(
        matches!(
            error,
            Some(BundleError::Count {
                declared: 2,
                found: 1
            })
        ),
        "{error:?}"
    );
    let future = text.replace(r#""version":2"#, r#""version":3"#);
    assert_ne!(future, text);
    let error = capture::bundle::parse(&future).err();
    assert_eq!(
        error.map(|error| error.to_string()),
        Some("session bundle version 3 is not supported".to_owned())
    );
    Ok(())
}

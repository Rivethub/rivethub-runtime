use sha2::Digest as _;
use sha2::Sha256;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::capture::UNKNOWN_ORIGIN;
use wire::capture::owned_import_key;
use wire::message::Role;

fn session(node: &str, conversation_id: &str, first_event: &str) -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: node.to_owned(),
            conversation_id: conversation_id.to_owned(),
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:shared".to_owned(),
            agent: "rivet".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: first_event.to_owned(),
                role: Role::User,
                content: "work".to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
    }
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn owned(owner: &str, session: &BundledSession) -> Result<String, Box<dyn std::error::Error>> {
    Ok(owned_import_key(owner, &session.import_batch()?.session_key)?.ok_or("an import key")?)
}

#[test]
fn an_import_names_its_origin_and_keeps_the_original_key_in_its_settings() -> TestResult {
    let known = session("desk", "c1", "evt-1");

    let batch = known.import_batch()?;

    assert_eq!(batch.session_key, r#"import:["desk","c1"]"#);
    let origin: SessionOrigin = batch
        .settings
        .as_ref()
        .and_then(|settings| settings.get("origin").cloned())
        .map(serde_json::from_value)
        .transpose()?
        .ok_or("no origin")?;
    assert_eq!(origin.session_key, "claude-code:shared");
    Ok(())
}

#[test]
fn the_receiver_key_hashes_the_owner_with_the_origin() -> TestResult {
    let known = session("desk", "c1", "evt-1");

    let key = owned("alex", &known)?;

    let digest = hex::encode(Sha256::digest(r#"["alex","desk","c1"]"#));
    assert_eq!(key, format!("import:{digest}"));
    assert_ne!(key, owned("bob", &known)?);
    Ok(())
}

#[test]
fn origins_that_join_to_the_same_text_get_different_keys() -> TestResult {
    assert_ne!(
        owned("team", &session("lab:x", "c", "evt-1"))?,
        owned("team", &session("lab", "x:c", "evt-1"))?
    );
    Ok(())
}

#[test]
fn a_version_one_key_is_a_stable_hash_of_what_the_session_carries() -> TestResult {
    let first = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-1");

    let key = owned("alex", &first)?;

    let digest = hex::encode(Sha256::digest(
        r#"["alex","unknown","claude-code:shared","rivet","evt-1"]"#,
    ));
    assert_eq!(key, format!("import:{digest}"));
    assert_ne!(
        key,
        owned("alex", &session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-9"))?
    );
    Ok(())
}

#[test]
fn a_key_that_does_not_name_an_origin_is_refused() {
    let hashed = format!("import:{}", "0".repeat(64));

    for refused in [
        hashed.as_str(),
        "import:[]",
        "import:desk:c1",
        r#"import:["a"]"#,
        r#"import:["desk","c1","evt-1"]"#,
        r#"import:["desk","claude-code:s","rivet","evt-1"]"#,
        r#"import:["unknown","claude-code:s","rivet","evt-1","x"]"#,
        r#"import:["unknown","c1"]"#,
    ] {
        assert!(owned_import_key("bob", refused).is_err(), "{refused}");
    }
    assert_eq!(
        owned_import_key("bob", "claude-code:s").ok().flatten(),
        None
    );
}

#[test]
fn re_exporting_an_undated_import_leaves_its_dates_absent() -> TestResult {
    let undated = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-1");
    let mut stored = BundledSession {
        origin: undated.origin.clone(),
        batch: undated.import_batch()?,
    };
    stored.batch.session_key = owned("alex", &undated)?;
    let imported_at = chrono::DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")?.to_utc();
    stored.batch.created_at = Some(imported_at);
    stored.batch.updated_at = Some(imported_at);

    let forwarded = stored.forwarding_stored_origin();

    assert_eq!(forwarded.batch.session_key, "claude-code:shared");
    assert_eq!(forwarded.batch.created_at, None);
    assert_eq!(forwarded.batch.updated_at, None);
    Ok(())
}

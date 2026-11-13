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
            identity: None,
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

#[test]
fn a_sanitized_identity_keys_the_conversation_instead_of_its_display_id() -> TestResult {
    let mut first = session("desk", "conv-<lan-ip>", "evt-1");
    let mut second = session("desk", "conv-<lan-ip>", "evt-2");
    assert_eq!(owned("alex", &first)?, owned("alex", &second)?);
    first.origin.identity = Some("a".repeat(64));
    second.origin.identity = Some("b".repeat(64));
    assert_ne!(owned("alex", &first)?, owned("alex", &second)?);
    let preimage = serde_json::to_string(&["alex", "desk", &"a".repeat(64)])?;
    assert_eq!(
        owned("alex", &first)?,
        format!("import:{}", hex::encode(Sha256::digest(preimage)))
    );
    Ok(())
}

fn receipt(fingerprint: Option<&str>) -> wire::capture::Receipt {
    wire::capture::Receipt {
        bundle_id: "AAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        received_at: chrono::DateTime::UNIX_EPOCH,
        verification: if fingerprint.is_some() {
            wire::capture::ReceiptVerification::Verified
        } else {
            wire::capture::ReceiptVerification::Unsigned
        },
        key_fingerprint: fingerprint.map(str::to_owned),
        checks: None,
    }
}

fn received(
    owner: &str,
    session: &BundledSession,
    fingerprint: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let batch = session.received_batch(&receipt(fingerprint))?;
    Ok(owned_import_key(owner, &batch.session_key)?.ok_or("an import key")?)
}

#[test]
fn a_received_conversation_is_keyed_by_its_signer_too() -> TestResult {
    let alice = session("desk", "c1", "evt-1");
    let forged = session("desk", "c1", "evt-9");
    let signed = received("ana", &alice, Some("ed25519:0011223344556677"))?;
    assert_eq!(
        signed,
        received("ana", &alice, Some("ed25519:0011223344556677"))?,
        "the same signer lands in the same conversation, so a re-share adds nothing"
    );
    assert_ne!(signed, received("ana", &forged, None)?);
    assert_ne!(
        signed,
        received("ana", &forged, Some("ed25519:8899aabbccddeeff"))?
    );
    assert_ne!(signed, owned("ana", &forged)?);
    let unknown = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-1");
    assert_ne!(received("ana", &unknown, None)?, owned("ana", &unknown)?);
    assert!(
        owned_import_key("ana", r#"import:["desk","c1","nobody"]"#).is_err(),
        "a third element must name a signer"
    );
    Ok(())
}

#[test]
fn a_stored_receipt_from_another_signer_refuses_the_merge() -> TestResult {
    let alice = session("desk", "c1", "evt-1");
    let stored = alice
        .received_batch(&receipt(Some("ed25519:0011223344556677")))?
        .settings
        .ok_or("settings")?;
    let mallory = alice
        .received_batch(&receipt(None))?
        .settings
        .ok_or("settings")?;
    let plain = alice.import_batch()?.settings.ok_or("settings")?;
    for incoming in [&mallory, &plain] {
        assert!(matches!(
            wire::capture::merge_imported_settings(Some(&stored), Some(incoming)),
            Err(wire::capture::MergeError::ReceiptConflict { .. })
        ));
    }
    assert!(wire::capture::merge_imported_settings(Some(&stored), Some(&stored)).is_ok());
    assert!(wire::capture::merge_imported_settings(Some(&plain), Some(&plain)).is_ok());
    Ok(())
}

#[test]
fn dropping_a_receipt_keeps_the_origin() -> TestResult {
    let receipt = wire::capture::Receipt {
        bundle_id: "rhb1:t".to_owned(),
        received_at: "2026-11-09T00:00:00Z".parse()?,
        verification: wire::capture::ReceiptVerification::Verified,
        key_fingerprint: Some("ed25519:abcd".to_owned()),
        checks: None,
    };
    let mut batch = session("desk", "c1", "evt-1").received_batch(&receipt)?;

    assert!(batch.drop_receipt());
    assert!(!batch.drop_receipt());

    let settings = batch.settings.ok_or("no settings")?;
    assert!(settings.get("receipt").is_none());
    assert!(settings.get("origin").is_some());
    Ok(())
}

use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::capture::UNKNOWN_ORIGIN;
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

#[test]
fn an_import_gets_its_own_session_key_and_keeps_the_original_in_its_origin()
-> Result<(), serde_json::Error> {
    let known = session("desk", "c1", "evt-1");

    let batch = known.import_batch()?;

    assert_eq!(batch.session_key, "import:desk:c1");
    let origin: SessionOrigin = batch
        .settings
        .as_ref()
        .and_then(|settings| settings.get("origin").cloned())
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_else(|| panic!("no origin in {batch:?}"));
    assert_eq!(origin.session_key, "claude-code:shared");
    Ok(())
}

#[test]
fn a_version_one_session_key_is_a_stable_hash_of_what_it_carries() -> Result<(), serde_json::Error>
{
    let first = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-1");
    let same = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-1");
    let other = session(UNKNOWN_ORIGIN, UNKNOWN_ORIGIN, "evt-9");

    let key = first.receiver_session_key()?;

    assert_eq!(key, same.receiver_session_key()?);
    assert!(key.starts_with("import:unknown:"), "{key}");
    assert_eq!(key.len(), "import:unknown:".len() + 32, "{key}");
    assert_ne!(key, other.receiver_session_key()?);
    Ok(())
}

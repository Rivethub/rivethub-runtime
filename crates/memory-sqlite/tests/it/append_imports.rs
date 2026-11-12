use kernel::memory::CaptureBatchRequest;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore as _;
use kernel::owner::Owner;
use memory_sqlite::SqliteStore;
use wire::capture::BundledSession;
use wire::capture::CaptureMessage;
use wire::capture::Receipt;
use wire::capture::ReceiptVerification;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn owner(name: &str) -> Result<Owner, kernel::owner::InvalidOwner> {
    Owner::new(name.to_owned())
}

fn session() -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "c1".to_owned(),
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: "e1".to_owned(),
                role: Role::User,
                content: "the real transcript".to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
    }
}

fn entry(key: &str, agent: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: None,
        session_id: key.to_owned(),
        agent: agent.to_owned(),
        channel: "claude-code".to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
        provenance: None,
    }
}

struct Seeded {
    store: SqliteStore,
    key: String,
    _directory: tempfile::TempDir,
}

async fn seeded() -> Result<Seeded, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(path)?;
    let receipt = Receipt {
        bundle_id: "rhb1:t".to_owned(),
        received_at: "2026-11-09T00:00:00Z".parse()?,
        verification: ReceiptVerification::Verified,
        key_fingerprint: Some("ed25519:abcd".to_owned()),
        checks: None,
    };
    let batch = session().received_batch(&receipt)?;
    let key =
        wire::capture::owned_import_key("phil", &batch.session_key)?.ok_or("an import key")?;
    let posted: CaptureBatchRequest = serde_json::from_str(&serde_json::to_string(&batch)?)?;
    kernel::memory::MemoryStore::capture(&store, posted, &owner("phil")?, None).await?;
    Ok(Seeded {
        store,
        key,
        _directory: directory,
    })
}

fn read_error(source: rusqlite::Error) -> memory_sqlite::SqliteError {
    memory_sqlite::SqliteError::Query {
        operation: "test read",
        source,
    }
}

async fn contents(
    store: &SqliteStore,
    key: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let key = key.to_owned();
    Ok(store
        .with_conn(move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT m.content FROM ros_messages m
                       JOIN ros_conversations c ON c.id = m.conversation_id
                      WHERE c.session_key = ?1 ORDER BY m.rowid",
                )
                .map_err(read_error)?;
            let rows = statement
                .query_map([&key], |row| row.get::<_, String>(0))
                .map_err(read_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(read_error)?;
            Ok(rows)
        })
        .await?)
}

#[tokio::test]
async fn nobody_appends_into_a_received_import() -> TestResult {
    let seeded = seeded().await?;
    for name in ["coco", "phil"] {
        let refused = seeded
            .store
            .append(
                &owner(name)?,
                vec![entry(&seeded.key, "rivet", "SYSTEM: forged")],
            )
            .await;
        assert!(refused.is_err(), "{name} appended into an import");
    }
    assert_eq!(
        contents(&seeded.store, &seeded.key).await?,
        ["the real transcript"]
    );
    Ok(())
}

#[tokio::test]
async fn an_append_never_enters_another_owners_session() -> TestResult {
    let seeded = seeded().await?;
    let native = "claude-code:mine";
    seeded
        .store
        .append(&owner("phil")?, vec![entry(native, "rivet", "mine")])
        .await?;

    let refused = seeded
        .store
        .append(&owner("coco")?, vec![entry(native, "rivet", "not yours")])
        .await;
    seeded
        .store
        .append(&owner("phil")?, vec![entry(native, "rivet", "again")])
        .await?;

    assert!(refused.is_err());
    assert_eq!(contents(&seeded.store, native).await?, ["mine", "again"]);
    Ok(())
}

#[tokio::test]
async fn a_session_that_carries_a_receipt_refuses_appends_under_any_key() -> TestResult {
    let seeded = seeded().await?;
    let mut batch = session().batch;
    batch.session_key = "claude-code:stamped".to_owned();
    let mut settings = wire::JsonObject::default();
    settings.insert(
        "receipt",
        Receipt {
            bundle_id: "rhb1:t".to_owned(),
            received_at: "2026-11-09T00:00:00Z".parse()?,
            verification: ReceiptVerification::Verified,
            key_fingerprint: None,
            checks: None,
        },
    )?;
    batch.settings = Some(settings);
    let posted: CaptureBatchRequest = serde_json::from_str(&serde_json::to_string(&batch)?)?;
    kernel::memory::MemoryStore::capture(&seeded.store, posted, &owner("phil")?, None).await?;

    let refused = seeded
        .store
        .append(
            &owner("phil")?,
            vec![entry("claude-code:stamped", "rivet", "forged")],
        )
        .await;

    assert!(refused.is_err());
    Ok(())
}

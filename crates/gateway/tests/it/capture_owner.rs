use crate::common;
use crate::session_support::user;

use std::sync::Arc;

use gateway::Gateway;
use gateway::tenancy::Tenancy;
use testkit::FakeMemoryStore;
use testkit::MemoryRequest;
use testkit::Request;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const COCO: &str = "coco-capture-token";

const BATCH: &str = r#"{"session_key":"cli:owned","agent":"rivet","channel":"cli","messages":[{"event_id":"e1","role":"user","content":"hello"}]}"#;

const RECEIPT: &str = r#"{"bundle_id":"rhb1:t","received_at":"2026-11-09T00:00:00Z","verification":"verified","key_fingerprint":"ed25519:abcd"}"#;

const IMPORT_KEY: &str = r#"import:[\"node\",\"conversation\"]"#;

const APPENDED: &str = r#"{"session_id":"cli:appended","role":"user","content":"hello"}"#;

fn post(target: &str, body: &str, token: Option<&str>) -> Request {
    let mut headers = vec![
        ("Host".to_owned(), "127.0.0.1:5174".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    if let Some(token) = token {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    Request {
        method: "POST".to_owned(),
        target: target.to_owned(),
        headers,
        body: body.as_bytes().to_vec(),
        recipe: None,
        via: None,
    }
}

async fn owners(store: &FakeMemoryStore) -> Vec<String> {
    store
        .requests()
        .await
        .into_iter()
        .filter_map(|request| {
            if let MemoryRequest::Append(owner, _) = request {
                Some(owner.as_str().to_owned())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn capture_and_append_record_the_authenticated_caller_as_owner() -> TestResult {
    let store = Arc::new(FakeMemoryStore::default());
    let tenancy = Tenancy::local(user("phil")).with_token(COCO, user("coco"))?;
    let gateway = Arc::new(
        Gateway::new(Arc::clone(&store) as _, common::settings())
            .with_local_owner(crate::common::local_owner())
            .with_tenancy(tenancy),
    );
    let app = gateway.router(false);
    for (target, body, token) in [
        ("/api/capture", BATCH, None),
        ("/api/capture", BATCH, Some(COCO)),
        ("/api/memory/tool/memory_append", APPENDED, None),
        ("/api/memory/tool/memory_append", APPENDED, Some(COCO)),
    ] {
        let response = common::answer(app.clone(), post(target, body, token)).await?;
        assert_eq!(
            response.status,
            200,
            "{target}: {}",
            String::from_utf8_lossy(&response.body)
        );
    }
    let unknown = common::answer(app, post("/api/capture", BATCH, Some("not-a-token"))).await?;
    assert_eq!(unknown.status, 401);
    assert_eq!(owners(&store).await, ["phil", "coco", "phil", "coco"]);
    Ok(())
}

fn imported(settings: &str) -> String {
    format!(
        r#"{{"session_key":"{IMPORT_KEY}","agent":"rivet","messages":[{{"event_id":"e1","role":"user","content":"hello"}}],"settings":{settings}}}"#
    )
}

fn received() -> String {
    imported(&format!(
        r#"{{"origin":{{"author":"Phil"}},"receipt":{RECEIPT}}}"#
    ))
}

fn member_gateway() -> Result<(Arc<FakeMemoryStore>, axum::Router), Box<dyn std::error::Error>> {
    let store = Arc::new(FakeMemoryStore::default());
    let tenancy = Tenancy::local(user("phil")).with_token(COCO, user("coco"))?;
    let gateway = Arc::new(
        Gateway::new(Arc::clone(&store) as _, common::settings())
            .with_local_owner(crate::common::local_owner())
            .with_tenancy(tenancy),
    );
    Ok((store, gateway.router(false)))
}

fn imported_key() -> Result<String, serde_json::Error> {
    serde_json::from_str(&format!("\"{IMPORT_KEY}\""))
}

#[tokio::test]
async fn a_member_import_is_stored_but_never_reads_as_verified() -> TestResult {
    let (store, app) = member_gateway()?;
    let native_receipt = format!(
        r#"{{"session_key":"cli:owned","agent":"rivet","messages":[{{"event_id":"e1","role":"user","content":"hello"}}],"settings":{{"receipt":{RECEIPT}}}}}"#
    );
    for (what, body) in [
        ("a forged receipt", received()),
        ("a receipt on a native key", native_receipt),
    ] {
        let response = common::answer(app.clone(), post("/api/capture", &body, Some(COCO))).await?;
        assert_eq!(
            response.status,
            200,
            "{what}: {}",
            String::from_utf8_lossy(&response.body)
        );
    }
    assert_eq!(owners(&store).await, ["coco", "coco"]);
    let key = imported_key()?;
    let origins = kernel::memory::MemoryStore::lookup_origins(&*store, vec![key.clone()]).await?;
    let provenance = origins.get(&key).ok_or("no provenance")?;
    assert!(!provenance.verification.is_verified());
    assert_eq!(provenance.author.as_deref(), Some("Phil"));
    Ok(())
}

#[tokio::test]
async fn the_owner_token_records_a_verified_receipt() -> TestResult {
    let (store, app) = member_gateway()?;
    let response = common::answer(app, post("/api/capture", &received(), None)).await?;
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let key = imported_key()?;
    let origins = kernel::memory::MemoryStore::lookup_origins(&*store, vec![key.clone()]).await?;
    let provenance = origins.get(&key).ok_or("no provenance")?;
    assert!(provenance.verification.is_verified());
    assert_eq!(provenance.author.as_deref(), Some("Phil"));
    Ok(())
}

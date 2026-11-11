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

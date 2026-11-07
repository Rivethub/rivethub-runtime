use crate::common;
use crate::session_support::user;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use gateway::Gateway;
use gateway::tenancy::Tenancy;
use serde_json::Value;
use testkit::Request;
use wire::events::SessionBroadcast;
use wire::gateway::GatewayError;
use wire::gateway::hub_state::HubStateResponse;

type Failure = Box<dyn std::error::Error>;
type TestResult = Result<(), Failure>;

const ALICE: &str = "alice-hub-token";
const ODD: &str = "odd-user-token";
const KEY: &str = "node-a::claude-code:00000000-0000-4000-8000-000000000001";
const OTHER: &str = "node-a::claude-code:00000000-0000-4000-8000-000000000002";

fn opened(root: &Path) -> Result<Arc<Gateway>, Failure> {
    let mut settings = common::settings();
    settings.state_dir = root.join("state");
    let tenancy = Tenancy::local(user("owner"))
        .with_token(ALICE, user("alice"))?
        .with_token(ODD, user("../odd/user"))?;
    Ok(Arc::new(
        Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings).with_tenancy(tenancy),
    ))
}

async fn exchange(
    app: &Router,
    method: &str,
    body: &str,
    token: Option<&str>,
) -> Result<testkit::Response, Failure> {
    let mut headers = vec![("Host".to_owned(), "127.0.0.1:5174".to_owned())];
    if let Some(token) = token {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    let request = Request {
        method: method.to_owned(),
        target: "/api/hub/state".to_owned(),
        headers,
        body: body.as_bytes().to_vec(),
        recipe: None,
        via: None,
    };
    let answered =
        tokio::time::timeout(Duration::from_secs(5), common::answer(app.clone(), request)).await?;
    Ok(answered?)
}

async fn get(app: &Router, token: Option<&str>) -> Result<HubStateResponse, Failure> {
    let response = exchange(app, "GET", "", token).await?;
    if response.status != 200 {
        return Err(format!(
            "GET {}: {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        )
        .into());
    }
    document(&response)
}

fn put(base: u64, state: &str) -> String {
    format!(r#"{{"baseVersion":{base},"state":{state}}}"#)
}

fn value(text: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_str(text)?)
}

fn document(response: &testkit::Response) -> Result<HubStateResponse, Failure> {
    serde_json::from_slice(&response.body)
        .map_err(|error| format!("{error}: {}", String::from_utf8_lossy(&response.body)).into())
}

#[tokio::test]
async fn starts_empty_at_version_zero() -> TestResult {
    let root = tempfile::tempdir()?;
    let app = opened(root.path())?.router(false);
    assert_eq!(get(&app, None).await?, HubStateResponse::default());
    Ok(())
}

#[tokio::test]
async fn a_put_advances_the_version_and_tells_the_users_sockets() -> TestResult {
    let root = tempfile::tempdir()?;
    let gateway = opened(root.path())?;
    let mut live = gateway.sessions().notices();
    let app = Arc::clone(&gateway).router(false);
    let state = format!(
        r#"{{"projects":[{{"id":"p1","name":"Code","order":0,"createdAt":1}}],"membership":{{"{KEY}":"p1"}},"names":{{"{KEY}":"Refactor"}},"lastSession":{{"key":"{KEY}","at":5}}}}"#
    );
    let saved = exchange(&app, "PUT", &put(0, &state), None).await?;
    assert_eq!(saved.status, 200);
    let saved = document(&saved)?;
    assert_eq!(saved.version, 1);
    assert_eq!(
        saved.state.membership.get(KEY).map(String::as_str),
        Some("p1")
    );
    assert_eq!(get(&app, None).await?, saved);

    let frame = tokio::time::timeout(Duration::from_secs(2), live.recv())
        .await??
        .body;
    assert!(matches!(&frame, SessionBroadcast::HubState(changed) if changed.version == 1));
    assert_eq!(
        serde_json::to_value(&frame)?,
        value(r#"{"type":"hub.state","v":1,"version":1}"#)?
    );

    let archived = format!(r#"{{"archived":["{KEY}"]}}"#);
    let next = exchange(&app, "PUT", &put(1, &archived), None).await?;
    assert_eq!(document(&next)?.version, 2);
    Ok(())
}

#[tokio::test]
async fn a_stale_put_conflicts_with_the_current_document() -> TestResult {
    let root = tempfile::tempdir()?;
    let app = opened(root.path())?.router(false);
    let pinned = format!(r#"{{"pinned":["{KEY}"]}}"#);
    let hidden = format!(r#"{{"hidden":["{KEY}"]}}"#);
    let first = exchange(&app, "PUT", &put(0, &pinned), None).await?;
    assert_eq!(first.status, 200);
    for stale in [0, 7] {
        let late = exchange(&app, "PUT", &put(stale, &hidden), None).await?;
        assert_eq!(late.status, 409);
        let current = document(&late)?;
        assert_eq!(current.version, 1);
        assert_eq!(current.state.pinned, vec![KEY.to_owned()]);
        assert_eq!(current.state.hidden, Vec::<String>::new());
    }
    Ok(())
}

#[tokio::test]
async fn the_stored_document_is_normalised() -> TestResult {
    let root = tempfile::tempdir()?;
    let app = opened(root.path())?.router(false);
    let state = format!(
        r#"{{
            "projects": [
                {{ "id": " p1 ", "name": " Code ", "order": 2.5, "createdAt": -4,
                   "defaults": {{ "directory": "relative/dir", "model": "  ", "effort": "high" }} }},
                {{ "id": "p1", "name": "Duplicate" }},
                {{ "id": "p2", "name": "   " }}
            ],
            "defaultProjectId": "gone",
            "membership": {{ "{KEY}": "p1", "{OTHER}": "gone", "no-separator": "p1" }},
            "names": {{ "{KEY}": "  Trimmed  ", "{OTHER}": "" }},
            "archived": ["{KEY}", "{KEY}", "::missing-node"],
            "modes": {{ "{KEY}": "terminal" }},
            "lastSession": {{ "key": "bad", "at": 1 }}
        }}"#
    );
    let saved = document(&exchange(&app, "PUT", &put(0, &state), None).await?)?;
    let expected = format!(
        r#"{{
            "projects": [{{ "id": "p1", "name": "Code", "order": 2.5, "createdAt": 0,
                            "defaults": {{ "effort": "high" }} }}],
            "membership": {{ "{KEY}": "p1" }},
            "names": {{ "{KEY}": "Trimmed" }},
            "archived": ["{KEY}"],
            "pinned": [],
            "hidden": [],
            "modes": {{ "{KEY}": "terminal" }}
        }}"#
    );
    assert_eq!(serde_json::to_value(&saved.state)?, value(&expected)?);
    Ok(())
}

#[tokio::test]
async fn an_oversized_or_malformed_document_is_refused() -> TestResult {
    let root = tempfile::tempdir()?;
    let app = opened(root.path())?.router(false);
    let pinned: Vec<String> = (0..501).map(|n| format!(r#""node-a::s{n}""#)).collect();
    let over_pinned = format!(r#"{{"pinned":[{}]}}"#, pinned.join(","));
    let over = exchange(&app, "PUT", &put(0, &over_pinned), None).await?;
    assert_eq!(over.status, 400);
    let error: GatewayError = serde_json::from_slice(&over.body)?;
    assert!(
        error.error.contains("too many pinned entries (max 500)"),
        "{}",
        error.error
    );

    for body in [
        "[1, 2]",
        r#"{"state":{}}"#,
        r#"{"baseVersion":-1,"state":{}}"#,
    ] {
        assert_eq!(
            exchange(&app, "PUT", body, None).await?.status,
            400,
            "{body}"
        );
    }
    assert_eq!(exchange(&app, "DELETE", "", None).await?.status, 405);
    assert_eq!(get(&app, None).await?.version, 0);
    Ok(())
}

#[tokio::test]
async fn each_user_has_their_own_document_and_it_survives_a_restart() -> TestResult {
    let root = tempfile::tempdir()?;
    {
        let app = opened(root.path())?.router(false);
        let pinned = format!(r#"{{"pinned":["{KEY}"]}}"#);
        let hidden = format!(r#"{{"hidden":["{KEY}"]}}"#);
        let saved = exchange(&app, "PUT", &put(0, &pinned), None).await?;
        assert_eq!(saved.status, 200);
        assert_eq!(get(&app, Some(ALICE)).await?.version, 0);
        let odd = exchange(&app, "PUT", &put(0, &hidden), Some(ODD)).await?;
        assert_eq!(odd.status, 200);
    }
    let app = opened(root.path())?.router(false);
    let owner = get(&app, None).await?;
    assert_eq!(owner.version, 1);
    assert_eq!(owner.state.pinned, vec![KEY.to_owned()]);
    assert_eq!(
        get(&app, Some(ODD)).await?.state.hidden,
        vec![KEY.to_owned()]
    );
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.path().join("state/hub-state"))? {
        files.push(entry?.file_name().to_string_lossy().into_owned());
    }
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(
        files
            .iter()
            .all(|name| !name.starts_with('.')
                && Path::new(name).extension() == Some("json".as_ref()))
    );
    Ok(())
}

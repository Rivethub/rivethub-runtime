use reqwest::Method;
use serde_json::Value;
use wire::gateway::memory::MemoryTagsAddRequest;
use wire::gateway::session_delete::SessionDeleteRequest;
use wire::gateway::tenancy::VisibilityUpdate;
use wire::gateway::topics::PlacementRequest;

use super::{ALEX, Hub, PHIL, SPACE, TOM, TestResult, body, listed, segment};

pub(super) const PHILS: &str = "claude-code:00000000-0000-4000-8000-00000000a001";
const ALEXS: &str = "claude-code:00000000-0000-4000-8000-00000000a002";
pub(super) const TOMS: &str = "claude-code:00000000-0000-4000-8000-00000000a003";
pub(super) const MARKER: &str = "quartzlantern";

fn private_mark() -> VisibilityUpdate {
    VisibilityUpdate {
        visibility: Some("private".to_owned()),
    }
}

fn tag_request() -> MemoryTagsAddRequest {
    MemoryTagsAddRequest {
        entity_type: Some("conversation".to_owned()),
        entity_id: None,
        session_key: Some(PHILS.to_owned()),
        agent: None,
        tag: Some("project:x".to_owned()),
        key: None,
        value: None,
        display: None,
        reason: None,
        decided_by: None,
    }
}

pub(super) async fn seeded() -> Result<Hub, Box<dyn std::error::Error>> {
    let hub = Hub::start().await?;
    hub.share_by_default(PHIL).await?;
    hub.share_by_default(ALEX).await?;
    let phil = format!("phil asks how the {MARKER} relay is wired into the hub");
    let tom = format!("tom keeps his notes on the {MARKER} relay to himself");
    hub.capture(PHIL, PHILS, &[&phil, "an answer"]).await?;
    hub.capture(ALEX, ALEXS, &["alex asks about the deploy", "an answer"])
        .await?;
    hub.capture(TOM, TOMS, &[&tom, "an answer"]).await?;
    Ok(hub)
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[tokio::test]
async fn each_person_lists_and_opens_their_own_and_the_space_s_sessions() -> TestResult {
    let hub = seeded().await?;
    for (person, expected) in [
        (PHIL, vec![PHILS, ALEXS]),
        (ALEX, vec![PHILS, ALEXS]),
        (TOM, vec![PHILS, ALEXS, TOMS]),
    ] {
        let reply = hub.get(person, "/api/sessions?limit=20").await?;
        assert_eq!(reply.status, 200, "{person}: {}", reply.body);
        assert_eq!(sorted(listed(&reply)), expected, "{person} lists");
    }
    for (person, session, status) in [
        (ALEX, ALEXS, 200),
        (ALEX, PHILS, 200),
        (ALEX, TOMS, 404),
        (TOM, TOMS, 200),
        (PHIL, TOMS, 404),
    ] {
        let reply = hub
            .get(person, &format!("/api/sessions/{}", segment(session)?))
            .await?;
        assert_eq!(
            reply.status, status,
            "{person} opens {session}: {}",
            reply.body
        );
    }
    let reply = hub
        .get(ALEX, &format!("/api/sessions/{}", segment(PHILS)?))
        .await?;
    assert_eq!(
        reply
            .body
            .pointer("/visibility/space")
            .and_then(Value::as_str),
        Some(SPACE),
        "{}",
        reply.body
    );
    let short = hub.get(ALEX, "/api/sessions?limit=1").await?;
    assert_eq!(listed(&short).len(), 1, "a page of one holds one session");
    let cursor = short
        .body
        .get("next")
        .and_then(Value::as_str)
        .ok_or("a second page")?;
    let rest = hub
        .get(ALEX, &format!("/api/sessions?limit=1&cursor={cursor}"))
        .await?;
    assert_eq!(listed(&rest).len(), 1, "the second page holds the other");
    hub.stop().await
}

#[tokio::test]
async fn only_the_owner_changes_a_session_and_a_reader_is_told_so() -> TestResult {
    let hub = seeded().await?;
    let path = format!("/api/sessions/{}", segment(PHILS)?);
    let changes = [
        (Method::PUT, format!("{path}/keep"), None),
        (Method::DELETE, format!("{path}/keep"), None),
        (Method::POST, format!("{path}/trash"), None),
        (Method::POST, format!("{path}/restore"), None),
        (
            Method::PATCH,
            format!("{path}/visibility"),
            Some(body(&private_mark())?),
        ),
        (
            Method::PUT,
            format!("{path}/placement"),
            Some(body(&PlacementRequest {
                topic_id: "project/anything".to_owned(),
            })?),
        ),
        (Method::DELETE, format!("{path}/placement"), None),
        (
            Method::POST,
            "/api/memory/tags/add".to_owned(),
            Some(body(&tag_request())?),
        ),
        (Method::DELETE, path.clone(), None),
    ];
    for (method, target, body) in &changes {
        let reply = hub.call(ALEX, method.clone(), target, body.clone()).await?;
        assert_eq!(reply.status, 403, "alex {method} {target}: {}", reply.body);
    }
    let bulk = hub
        .call(
            ALEX,
            Method::POST,
            "/api/sessions/delete",
            Some(body(&SessionDeleteRequest {
                sessions: vec![PHILS.parse()?],
            })?),
        )
        .await?;
    assert_eq!(
        bulk.body
            .pointer("/results/0/error")
            .and_then(Value::as_str),
        Some("not owner"),
        "{}",
        bulk.body
    );
    let private = format!("/api/sessions/{}/visibility", segment(TOMS)?);
    let reply = hub
        .call(ALEX, Method::PATCH, &private, Some(body(&private_mark())?))
        .await?;
    assert_eq!(
        reply.status, 404,
        "a session alex cannot read: {}",
        reply.body
    );
    let stray: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_session_marks WHERE owner_user_id = 'alex'")
            .fetch_one(&hub.db.pool())
            .await?;
    assert_eq!(stray, 0, "a refused mark stores nothing");
    hub.stop().await
}

#[tokio::test]
async fn the_owner_changes_a_session_and_readers_see_the_change() -> TestResult {
    let hub = seeded().await?;
    let path = format!("/api/sessions/{}", segment(PHILS)?);
    for (method, target) in [
        (Method::PUT, format!("{path}/keep")),
        (Method::DELETE, format!("{path}/keep")),
        (Method::POST, format!("{path}/trash")),
        (Method::POST, format!("{path}/restore")),
    ] {
        let reply = hub.call(PHIL, method.clone(), &target, None).await?;
        assert_eq!(reply.status, 204, "phil {method} {target}: {}", reply.body);
    }
    let reply = hub
        .call(
            PHIL,
            Method::PATCH,
            &format!("{path}/visibility"),
            Some(body(&private_mark())?),
        )
        .await?;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(
        !listed(&hub.get(ALEX, "/api/sessions").await?).contains(&PHILS.to_owned()),
        "phil's private mark takes the session from alex"
    );
    hub.stop().await
}

async fn sees(
    hub: &Hub,
    person: &str,
    session: &str,
) -> Result<[bool; 3], Box<dyn std::error::Error>> {
    let listed =
        listed(&hub.get(person, "/api/sessions?limit=50").await?).contains(&session.to_owned());
    let search = hub
        .get(person, &format!("/api/memory/search?q={MARKER}&limit=50"))
        .await?
        .body
        .to_string()
        .contains(session);
    let browse = hub
        .get(person, "/api/memory/browse?limit=200")
        .await?
        .body
        .to_string()
        .contains(session);
    Ok([listed, search, browse])
}

#[tokio::test]
async fn trash_and_restore_are_seen_by_everyone_the_session_is_shared_with() -> TestResult {
    let hub = seeded().await?;
    for person in [PHIL, ALEX, TOM] {
        assert_eq!(
            sees(&hub, person, PHILS).await?,
            [true; 3],
            "{person} before"
        );
    }
    let trash = format!("/api/sessions/{}/trash", segment(PHILS)?);
    assert_eq!(
        hub.call(PHIL, Method::POST, &trash, None).await?.status,
        204
    );
    for person in [PHIL, ALEX, TOM] {
        assert_eq!(
            sees(&hub, person, PHILS).await?,
            [false; 3],
            "{person} after trash"
        );
        let trashed = listed(&hub.get(person, "/api/sessions?view=trash").await?);
        assert_eq!(trashed, [PHILS], "{person} sees it in the trash view");
    }
    let restore = format!("/api/sessions/{}/restore", segment(PHILS)?);
    assert_eq!(
        hub.call(PHIL, Method::POST, &restore, None).await?.status,
        204
    );
    for person in [PHIL, ALEX, TOM] {
        assert_eq!(
            sees(&hub, person, PHILS).await?,
            [true; 3],
            "{person} after restore"
        );
    }
    hub.stop().await
}

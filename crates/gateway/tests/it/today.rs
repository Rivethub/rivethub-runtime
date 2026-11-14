#![cfg(test)]
use crate::common;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use common::answer;
use gateway::Gateway;
use kernel::changes::{
    ChangeError, ChangeFeed, ChangeRecord, ChangeWindow, DailyDigest, DigestRequest, DigestStore,
    TopicRef,
};
use kernel::memory::{MemoryState, MemoryStore, SessionRecord};
use kernel::owner::Owner;
use kernel::topics::{PlacementSource, TopicId, TopicKind};
use parking_lot::Mutex;
use testkit::{FakeSessionCatalog, FakeSessionDeletion, Request};
use wire::gateway::review::SubjectRef;
use wire::gateway::today::{Change, TodayDigest};
use wire::session_id::SessionId;

const BACKUPS: &str = "00000000-0000-4000-8000-0000000000a1";
const NAS: &str = "00000000-0000-4000-8000-0000000000a2";
const SESSION: &str = "claude-code:00000000-0000-4000-8000-0000000000c1";
const OTHER: &str = "claude-code:00000000-0000-4000-8000-0000000000c2";

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn session(key: &str) -> SessionId {
    key.parse().unwrap()
}

fn topic(id: &str, title: &str, kind: TopicKind) -> TopicRef {
    TopicRef {
        id: id.parse::<TopicId>().unwrap(),
        title: title.to_owned(),
        kind,
    }
}

struct Feed(Vec<ChangeRecord>);

#[async_trait]
impl ChangeFeed for Feed {
    async fn records(&self, _window: &ChangeWindow) -> Result<Vec<ChangeRecord>, ChangeError> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct Digests {
    kept: Mutex<Option<DailyDigest>>,
    requested: Mutex<Vec<DigestRequest>>,
}

#[async_trait]
impl DigestStore for Digests {
    async fn digest(
        &self,
        day: NaiveDate,
        owner: &Owner,
    ) -> Result<Option<DailyDigest>, ChangeError> {
        Ok(self
            .kept
            .lock()
            .clone()
            .filter(|digest| digest.day == day && digest.owner == *owner))
    }

    async fn keep_digest(&self, digest: &DailyDigest) -> Result<(), ChangeError> {
        *self.kept.lock() = Some(digest.clone());
        Ok(())
    }

    async fn request_digest(&self, request: &DigestRequest) -> Result<(), ChangeError> {
        self.requested.lock().push(request.clone());
        Ok(())
    }
}

fn record(key: &str, title: &str, started: &str) -> SessionRecord {
    SessionRecord {
        owner: None,
        id: session(key),
        title: title.to_owned(),
        started_at: at(started),
        ended_at: Some(at(started)),
        message_count: 6,
        topic: None,
        kept: false,
        trashed: false,
        memory: MemoryState::Summarized,
        gist: None,
        cwd: None,
        origin: None,
    }
}

fn records() -> Vec<ChangeRecord> {
    vec![
        ChangeRecord::Created {
            topic: topic(BACKUPS, "Backups", TopicKind::Work),
            at: at("2026-09-21T12:05:00Z"),
        },
        ChangeRecord::Decision {
            topic: topic(BACKUPS, "Backups", TopicKind::Work),
            text: "Keep 14 days of nightly dumps".to_owned(),
            session: Some(session(SESSION)),
            at: at("2026-09-21T12:10:00Z"),
        },
        ChangeRecord::Placed {
            topic: topic(BACKUPS, "Backups", TopicKind::Work),
            session: session(SESSION),
            by: PlacementSource::Categorizer,
            at: at("2026-09-21T12:20:00Z"),
        },
        ChangeRecord::Created {
            topic: topic(NAS, "nas-01", TopicKind::Entity),
            at: at("2026-09-18T12:00:00Z"),
        },
    ]
}

fn app(digests: Arc<Digests>) -> axum::Router {
    let catalog = FakeSessionCatalog::new(Arc::new(FakeSessionDeletion::default()))
        .with_record(record(
            SESSION,
            "Set up nightly backups",
            "2026-09-21T12:00:00Z",
        ))
        .with_record(record(OTHER, "Yesterday's work", "2026-09-20T12:00:00Z"));
    let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
    let owner = gateway::tenancy::UserId::new("owner".to_owned()).unwrap();
    let gateway = Gateway::new(store, common::settings())
        .with_local_owner(crate::common::local_owner())
        .with_tenancy(gateway::tenancy::Tenancy::local(owner.clone()))
        .with_sessions(Arc::new(
            gateway::sessions::SessionRegistry::default().with_node_owner(owner),
        ))
        .with_session_catalog(Arc::new(catalog))
        .with_changes(Arc::new(Feed(records())), digests);
    Arc::new(gateway).router(false)
}

fn get(target: &str) -> Request {
    Request {
        method: "GET".to_owned(),
        target: target.to_owned(),
        headers: vec![("Host".to_owned(), "127.0.0.1:5174".to_owned())],
        body: Vec::new(),
        recipe: None,
        via: None,
    }
}

async fn today(app: axum::Router, date: &str) -> TodayDigest {
    let response = answer(app, get(&format!("/api/today?date={date}")))
        .await
        .unwrap();
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).unwrap()
}

#[tokio::test]
async fn today_is_the_days_sessions_with_the_changes_they_caused() {
    let digests = Arc::new(Digests::default());
    let digest = today(app(digests.clone()), "2026-09-21").await;
    assert_eq!(digest.date, "2026-09-21".parse::<NaiveDate>().unwrap());
    assert_eq!(
        digest.headline,
        "Your agents learned 2 things on Monday, September 21"
    );
    assert_eq!(
        digest.summary,
        format!(
            "Most of the work was on [Backups](/topics/{BACKUPS}). \
             The day brought 1 decision, 1 new page and 1 session filed."
        )
    );
    assert_eq!(digest.needs_you, []);
    assert_eq!(digest.review_count, 0);
    let [entry] = digest.entries.as_slice() else {
        panic!("one session that day: {:?}", digest.entries);
    };
    assert_eq!(entry.session.title, "Set up nightly backups");
    assert_eq!(
        entry
            .changes
            .iter()
            .map(|change| change.text.as_str())
            .collect::<Vec<_>>(),
        ["Filed under Backups.", "Keep 14 days of nightly dumps"]
    );
    assert_eq!(
        entry
            .changes
            .first()
            .and_then(|change| change.source.as_ref())
            .map(|source| source.title.as_str()),
        Some("Set up nightly backups")
    );
    assert_eq!(digest.activity_by_day.len(), 30);
    let level = |day: &str| {
        digest
            .activity_by_day
            .iter()
            .find(|entry| entry.date == day.parse::<NaiveDate>().unwrap())
            .map(|entry| entry.level)
    };
    assert_eq!(level("2026-09-21"), Some(4));
    assert_eq!(level("2026-09-18"), Some(2));
    assert_eq!(level("2026-09-20"), Some(0));
    assert_eq!(
        digest.recently_updated,
        [
            SubjectRef::Topic {
                id: BACKUPS.to_owned(),
                title: "Backups".to_owned()
            },
            SubjectRef::Entity {
                id: NAS.to_owned(),
                title: "nas-01".to_owned()
            }
        ]
    );
    let requested = digests.requested.lock().clone();
    let [request] = requested.as_slice() else {
        panic!("a day with changes and no digest queues one: {requested:?}");
    };
    assert_eq!(request.day.to_string(), "2026-09-21");
}

#[tokio::test]
async fn a_kept_digest_is_the_summary_and_is_not_asked_for_again_while_current() {
    let digests = Arc::new(Digests::default());
    digests
        .keep_digest(&DailyDigest {
            day: "2026-09-21".parse().unwrap(),
            owner: Owner::new("owner".to_owned()).unwrap(),
            summary: "The day set up [Backups](/topics/x).".to_owned(),
            changes: 3,
            written_at: at("2026-09-21T13:00:00Z"),
        })
        .await
        .unwrap();
    let digest = today(app(digests.clone()), "2026-09-21").await;
    assert_eq!(digest.summary, "The day set up [Backups](/topics/x).");
    assert_eq!(*digests.requested.lock(), []);
}

#[tokio::test]
async fn a_quiet_day_says_so_and_queues_nothing() {
    let digests = Arc::new(Digests::default());
    let digest = today(app(digests.clone()), "2026-09-20").await;
    assert_eq!(digest.headline, "A quiet day");
    assert_eq!(
        digest.summary,
        "1 session was captured. Nothing new was learned from it yet."
    );
    let [entry] = digest.entries.as_slice() else {
        panic!("one session that day: {:?}", digest.entries);
    };
    assert_eq!(entry.changes, []);
    assert_eq!(*digests.requested.lock(), []);
}

#[tokio::test]
async fn the_change_feed_answers_a_subjects_history() {
    let app = app(Arc::new(Digests::default()));
    let response = answer(
        app.clone(),
        get(&format!(
            "/api/changes?from=2026-09-01T00:00:00Z&to=2026-10-01T00:00:00Z&subject={SESSION}"
        )),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    let changes: Vec<Change> = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        changes
            .iter()
            .map(|change| change.text.as_str())
            .collect::<Vec<_>>(),
        ["Filed under Backups.", "Keep 14 days of nightly dumps"]
    );
    let bad = answer(app, get("/api/changes?subject=not-a-subject"))
        .await
        .unwrap();
    assert_eq!(bad.status, 400);
}

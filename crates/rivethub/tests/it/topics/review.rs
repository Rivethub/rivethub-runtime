use axum::http::StatusCode;
use chrono::DateTime;
use chrono::SubsecRound as _;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::facts::Evidence;
use kernel::facts::FactKey;
use kernel::facts::FactStore as _;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::owner::Owner;
use kernel::tidy::Batch;
use kernel::tidy::Evidence as TidyEvidence;
use kernel::tidy::Proposer;
use kernel::tidy::Reason;
use kernel::tidy::Sample;
use kernel::tidy::TidyStore as _;
use kernel::tidy::TreeEdit;
use kernel::topics::Suggestion;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use kernel::topics::TopicStore as _;
use uuid::Uuid;
use wire::gateway::entities::EntityArticle;
use wire::gateway::memory::PendingTagListResult;
use wire::gateway::review::ReviewGroup;
use wire::gateway::review::ReviewItem;
use wire::gateway::review::SubjectRef;
use wire::gateway::today::TodayDigest;
use wire::gateway::topics::PlacedBy;
use wire::gateway::topics::PlacementReply;
use wire::gateway::topics::TopicList;
use wire::gateway::topics::TopicRecord;
use wire::session_id::SessionId;

use super::Api;
use super::TestResult;
use super::placement_path;

const FIRST: &str = "claude-code:00000000-0000-4000-8000-0000000000d1";
const SECOND: &str = "claude-code:00000000-0000-4000-8000-0000000000d2";
const UNFILED: &str = "claude-code:00000000-0000-4000-8000-0000000000d3";
const TAG: &str = "00000000-0000-4000-8000-0000000000d4";

struct Seeded {
    host: TopicRecord,
    lab: TopicRecord,
    stale: TopicRecord,
}

async fn entity(api: &Api, name: &str, kind: &str) -> TestResult<TopicRecord> {
    let types: TopicList = api
        .json("GET", "/api/topics?kind=type", "", StatusCode::OK)
        .await?;
    let parent = types
        .topics
        .iter()
        .find(|summary| summary.topic.name == kind)
        .ok_or("the seeded type is missing")?
        .topic
        .id
        .clone();
    api.json(
        "POST",
        "/api/topics",
        &format!(r#"{{"kind":"entity","name":"{name}","parent":"{parent}"}}"#),
        StatusCode::CREATED,
    )
    .await
}

fn said(
    entity: &TopicRecord,
    key: &str,
    value: &str,
    session: &str,
    words: &str,
    at: DateTime<Utc>,
) -> TestResult<NewClaim> {
    Ok(NewClaim {
        entity: entity.id.parse::<TopicId>()?,
        key: FactKey::new(key)?,
        value: FactValue::new(value)?,
        said: Said::Writer {
            session: session.parse::<SessionId>()?,
            summary: Uuid::new_v4(),
            evidence: Evidence::quote(words),
            settled: false,
        },
        owner: Some(Owner::new("owner".to_owned())?),
        at,
    })
}

async fn seed(api: &Api) -> TestResult<Seeded> {
    let now = Utc::now().trunc_subsecs(3);
    let host = entity(api, "web01", "Hosts").await?;
    for (key, title) in [(FIRST, "Upgrade web01"), (SECOND, "Audit the fleet")] {
        testkit::filing::capture(api.store.as_ref(), key, "claude-code", title, None).await?;
    }
    let conversation = testkit::filing::capture(
        api.store.as_ref(),
        UNFILED,
        "claude-code",
        "Back up the NAS nightly",
        None,
    )
    .await?;
    api.store
        .assert_facts(&[
            said(
                &host,
                "os",
                "Ubuntu 22.04",
                SECOND,
                "web01 still runs Ubuntu 22.04",
                now - TimeDelta::hours(3),
            )?,
            said(
                &host,
                "os",
                "Ubuntu 24.04",
                FIRST,
                "web01 is on Ubuntu 24.04 now",
                now - TimeDelta::hours(2),
            )?,
            said(
                &host,
                "address",
                "192.0.2.4",
                FIRST,
                "web01 answers on 192.0.2.4",
                now - TimeDelta::hours(1),
            )?,
        ])
        .await?;
    let (lab, stale) = seed_proposals(api, conversation, now).await?;
    Ok(Seeded { host, lab, stale })
}

async fn seed_proposals(
    api: &Api,
    conversation: Uuid,
    now: DateTime<Utc>,
) -> TestResult<(TopicRecord, TopicRecord)> {
    let lab: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Home Lab"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let stale: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Old Experiments"}"#,
            StatusCode::CREATED,
        )
        .await?;
    api.store
        .suggest(Suggestion {
            session: UNFILED.parse()?,
            name: TopicName::new("Home Lab")?,
            topic: Some(lab.id.parse()?),
            parent: None,
        })
        .await?;
    api.database()?.execute(
        "INSERT INTO ros_tags (id, entity_type, entity_id, key, value, display, source, state,
                               proposed_by, reason, created_at, updated_at)
         VALUES (?1, 'conversation', ?2, 'area', 'storage', 'area: storage', 'model',
                 'suggested', 'categorizer', 'backups', ?3, ?3)",
        rusqlite::params![TAG, conversation.to_string(), now.to_rfc3339()],
    )?;
    let mut batch = Batch::new(Proposer::Categorizer, now - TimeDelta::minutes(30));
    batch.add(
        TreeEdit::Close {
            topic: stale.id.parse()?,
        },
        Reason::new("Nothing has happened there for months")?,
        TidyEvidence {
            sessions: 1,
            samples: vec![Sample {
                id: SECOND.to_owned(),
                title: Some("Audit the fleet".to_owned()),
                last_active: Some(now - TimeDelta::hours(3)),
            }],
        },
    );
    api.store.propose(batch).await?;
    Ok((lab, stale))
}

async fn fact_value(api: &Api, host: &TopicRecord, key: &str) -> TestResult<(String, bool)> {
    let article: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    let fact = article
        .facts
        .iter()
        .find(|fact| fact.key == key)
        .ok_or_else(|| format!("no {key} fact"))?;
    Ok((fact.value.clone(), fact.conflict.is_some()))
}

async fn queue(api: &Api) -> TestResult<Vec<ReviewItem>> {
    api.json("GET", "/api/review", "", StatusCode::OK).await
}

fn find(items: &[ReviewItem], group: ReviewGroup) -> TestResult<ReviewItem> {
    Ok(items
        .iter()
        .find(|item| item.group == group)
        .cloned()
        .ok_or_else(|| format!("no {group:?} item in {items:?}"))?)
}

async fn decide(api: &Api, item: &ReviewItem, body: &str) -> TestResult<StatusCode> {
    let (status, bytes) = api
        .call("POST", &format!("/api/review/{}/decision", item.id), body)
        .await?;
    if status != StatusCode::NO_CONTENT {
        return Err(format!("{}: {status} {}", item.id, String::from_utf8_lossy(&bytes)).into());
    }
    Ok(status)
}

#[tokio::test]
async fn the_queue_gathers_every_proposal_newest_first() -> TestResult {
    let api = Api::new(true)?;
    let seeded = seed(&api).await?;
    let items = queue(&api).await?;
    let groups: Vec<ReviewGroup> = items.iter().map(|item| item.group).collect();
    assert_eq!(groups.len(), 5, "{items:?}");
    for group in [
        ReviewGroup::Conflict,
        ReviewGroup::Fact,
        ReviewGroup::Filing,
        ReviewGroup::Tag,
        ReviewGroup::Tidy,
    ] {
        assert!(groups.contains(&group), "{group:?} is in the queue");
    }
    assert!(
        items
            .windows(2)
            .all(|pair| pair.first().map(|item| item.created_at)
                >= pair.get(1).map(|item| item.created_at)),
        "newest first"
    );

    let conflict = find(&items, ReviewGroup::Conflict)?;
    assert_eq!(conflict.text, "os");
    assert_eq!(
        conflict.subject,
        SubjectRef::Entity {
            id: seeded.host.id.clone(),
            title: "web01".to_owned()
        }
    );
    let options: Vec<&str> = conflict
        .options
        .iter()
        .flatten()
        .map(|option| option.value.as_str())
        .collect();
    assert_eq!(options, ["Ubuntu 24.04", "Ubuntu 22.04"]);
    let quotes: Vec<(&str, Option<&str>)> = conflict
        .evidence
        .iter()
        .map(|evidence| (evidence.quote.as_str(), evidence.highlight.as_deref()))
        .collect();
    assert_eq!(
        quotes,
        [
            ("web01 is on Ubuntu 24.04 now", Some("Ubuntu 24.04")),
            ("web01 still runs Ubuntu 22.04", Some("Ubuntu 22.04")),
        ]
    );

    let fact = find(&items, ReviewGroup::Fact)?;
    assert_eq!(fact.text, "address: 192.0.2.4");
    assert_eq!(fact.after.as_deref(), Some("192.0.2.4"));

    let filing = find(&items, ReviewGroup::Filing)?;
    assert_eq!(filing.text, "→ Home Lab");
    assert_eq!(filing.after.as_deref(), Some(seeded.lab.id.as_str()));
    assert_eq!(filing.id, format!("filing.{UNFILED}.{}", seeded.lab.id));

    let tag = find(&items, ReviewGroup::Tag)?;
    assert_eq!(tag.id, format!("tag.{TAG}"));
    assert_eq!(tag.text, "Add area: storage");

    let tidy = find(&items, ReviewGroup::Tidy)?;
    assert_eq!(
        tidy.text,
        "Close “Old Experiments”. Nothing has happened there for months"
    );
    assert_eq!(
        tidy.subject,
        SubjectRef::Topic {
            id: seeded.stale.id,
            title: "Old Experiments".to_owned()
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_conflict_is_settled_by_your_choice_once() -> TestResult {
    let api = Api::new(true)?;
    let seeded = seed(&api).await?;
    let items = queue(&api).await?;
    let conflict = find(&items, ReviewGroup::Conflict)?;
    let (status, _) = api
        .error(
            "POST",
            &format!("/api/review/{}/decision", conflict.id),
            r#"{"kind":"choose","value":"Windows"}"#,
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a choice must be an option"
    );
    decide(
        &api,
        &conflict,
        r#"{"kind":"choose","value":"Ubuntu 22.04"}"#,
    )
    .await?;
    decide(
        &api,
        &conflict,
        r#"{"kind":"choose","value":"Ubuntu 22.04"}"#,
    )
    .await?;
    assert_eq!(
        fact_value(&api, &seeded.host, "os").await?,
        ("Ubuntu 22.04".to_owned(), false),
        "your choice holds and resolves the conflict"
    );
    let fact = find(&items, ReviewGroup::Fact)?;
    decide(&api, &fact, r#"{"kind":"later"}"#).await?;
    let left: Vec<ReviewGroup> = queue(&api).await?.iter().map(|item| item.group).collect();
    assert_eq!(
        left,
        [ReviewGroup::Tag, ReviewGroup::Tidy, ReviewGroup::Filing],
        "the settled conflict and the snoozed fact leave the queue"
    );
    Ok(())
}

#[tokio::test]
async fn filing_tags_and_tidy_go_through_their_own_stores() -> TestResult {
    let api = Api::new(true)?;
    let seeded = seed(&api).await?;
    let items = queue(&api).await?;

    decide(
        &api,
        &find(&items, ReviewGroup::Filing)?,
        r#"{"kind":"accept"}"#,
    )
    .await?;
    let placed: PlacementReply = api
        .json(
            "GET",
            &placement_path(&UNFILED.parse()?),
            "",
            StatusCode::OK,
        )
        .await?;
    let placement = placed.placement.ok_or("the session is filed")?;
    assert_eq!(placement.topic_id, seeded.lab.id);
    assert_eq!(placement.by, PlacedBy::You);

    let tag = find(&items, ReviewGroup::Tag)?;
    let (status, _) = api
        .error(
            "POST",
            &format!("/api/review/{}/decision", tag.id),
            r#"{"kind":"choose","value":"x"}"#,
        )
        .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    decide(&api, &tag, r#"{"kind":"accept"}"#).await?;
    let pending: PendingTagListResult = api
        .json("GET", "/api/memory/tags/pending", "", StatusCode::OK)
        .await?;
    assert_eq!(pending.tags, Vec::new(), "the tag was accepted");

    decide(
        &api,
        &find(&items, ReviewGroup::Tidy)?,
        r#"{"kind":"accept"}"#,
    )
    .await?;
    let closed: TopicRecord = api
        .json(
            "GET",
            &format!("/api/topics/{}", seeded.stale.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert!(closed.closed_at.is_some(), "the proposal closed the topic");

    for missing in ["tag.00000000-0000-4000-8000-0000000000ff", "nonsense"] {
        let (status, _) = api
            .error(
                "POST",
                &format!("/api/review/{missing}/decision"),
                r#"{"kind":"accept"}"#,
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }
    Ok(())
}

#[tokio::test]
async fn editing_a_new_fact_records_your_value() -> TestResult {
    let api = Api::new(true)?;
    let seeded = seed(&api).await?;
    let fact = find(&queue(&api).await?, ReviewGroup::Fact)?;
    decide(&api, &fact, r#"{"kind":"edit","value":"192.0.2.40"}"#).await?;
    assert_eq!(
        fact_value(&api, &seeded.host, "address").await?,
        ("192.0.2.40".to_owned(), false)
    );
    assert!(
        queue(&api)
            .await?
            .iter()
            .all(|item| item.group != ReviewGroup::Fact),
        "a value you set is not new"
    );
    Ok(())
}

#[tokio::test]
async fn today_shows_three_review_items_conflicts_first_and_counts_the_queue() -> TestResult {
    let api = Api::new(true)?;
    seed(&api).await?;
    api.database()?.execute(
        "INSERT INTO ros_tags (id, entity_type, entity_id, key, value, display, source, state,
                               proposed_by, reason, created_at, updated_at)
         SELECT ?1, entity_type, entity_id, 'area', 'backups', 'area: backups', source, state,
                proposed_by, reason, ?2, ?2
           FROM ros_tags WHERE id = ?3",
        rusqlite::params![
            "00000000-0000-4000-8000-0000000000d5",
            (Utc::now() + TimeDelta::hours(1)).to_rfc3339(),
            TAG
        ],
    )?;
    let items = queue(&api).await?;
    let today: TodayDigest = api.json("GET", "/api/today", "", StatusCode::OK).await?;
    assert_eq!(today.review_count, 6);
    let groups: Vec<ReviewGroup> = today.needs_you.iter().map(|item| item.group).collect();
    assert_eq!(groups.len(), 3, "{groups:?}");
    assert_eq!(groups.first(), Some(&ReviewGroup::Conflict));
    assert_ne!(
        items.first().map(|item| item.group),
        Some(ReviewGroup::Conflict),
        "the queue alone does not lead with the conflict"
    );
    let rest: Vec<ReviewItem> = items
        .iter()
        .filter(|item| item.group != ReviewGroup::Conflict)
        .take(2)
        .cloned()
        .collect();
    assert_eq!(
        today.needs_you.get(1..),
        Some(rest.as_slice()),
        "after the conflict, the queue's newest items"
    );

    let conflict = find(&items, ReviewGroup::Conflict)?;
    decide(&api, &conflict, r#"{"kind":"later"}"#).await?;
    let today: TodayDigest = api.json("GET", "/api/today", "", StatusCode::OK).await?;
    assert_eq!(today.review_count, 5, "a snoozed item leaves Today too");
    assert!(
        today.needs_you.iter().all(|item| item.id != conflict.id),
        "{:?}",
        today.needs_you
    );
    Ok(())
}

#[tokio::test]
async fn review_and_today_look_up_only_the_suggested_sessions_in_one_call() -> TestResult {
    let api = Api::new(true)?;
    let seeded = seed(&api).await?;
    for key in [FIRST, SECOND] {
        api.store
            .suggest(Suggestion {
                session: key.parse()?,
                name: TopicName::new("Home Lab")?,
                topic: Some(seeded.lab.id.parse()?),
                parent: None,
            })
            .await?;
    }
    for number in 0..12_u8 {
        let key = format!("codex:00000000-0000-4000-8000-0000000001{number:02x}");
        testkit::filing::capture(api.store.as_ref(), &key, "codex", "Unrelated work", None).await?;
    }
    api.lookups.take();
    let items = queue(&api).await?;
    assert_eq!(
        items
            .iter()
            .filter(|item| item.group == ReviewGroup::Filing)
            .count(),
        3
    );
    let calls = api.lookups.take();
    assert_eq!(calls.lists, 0, "review never pages the catalog");
    let looked_up: Vec<std::collections::BTreeSet<String>> = calls
        .records
        .iter()
        .map(|sessions| sessions.iter().map(ToString::to_string).collect())
        .collect();
    assert_eq!(
        looked_up,
        vec![
            [FIRST, SECOND, UNFILED]
                .into_iter()
                .map(str::to_owned)
                .collect()
        ],
        "one lookup of exactly the suggested sessions"
    );
    let _today: TodayDigest = api.json("GET", "/api/today", "", StatusCode::OK).await?;
    let calls = api.lookups.take();
    assert_eq!(calls.lists, 1, "one page covers a quiet day");
    assert!(
        calls.records.len() <= 3,
        "sources, titles and filing each look up once: {calls:?}"
    );
    Ok(())
}

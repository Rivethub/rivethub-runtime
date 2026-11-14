use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::EntityHistory;
use kernel::facts::Evidence;
use kernel::facts::FactError;
use kernel::facts::FactKey;
use kernel::facts::FactSession;
use kernel::facts::FactStore;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::SessionLink;
use kernel::memory::Provenance;
use kernel::owner::Owner;
use kernel::topics::TopicId;
use memory_sqlite::SqliteStore;
use uuid::Uuid;
use wire::gateway::entities::EntityArticle;
use wire::gateway::entities::EntitySummary;
use wire::gateway::entities::EntityTypeInfo;
use wire::gateway::entities::Fact;
use wire::gateway::topics::TopicList;
use wire::gateway::topics::TopicRecord;
use wire::memory::Verification as ImportVerification;
use wire::session_id::SessionId;

use super::Api;
use super::TestResult;

const FIRST: &str = "claude-code:00000000-0000-4000-8000-0000000000e1";
const SECOND: &str = "claude-code:00000000-0000-4000-8000-0000000000e2";

fn hour(count: i64) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + TimeDelta::days(20_400) + TimeDelta::hours(count)
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
    at: i64,
) -> TestResult<NewClaim> {
    Ok(NewClaim {
        entity: entity.id.parse::<TopicId>()?,
        key: FactKey::new(key)?,
        value: FactValue::new(value)?,
        said: Said::Writer {
            session: session.parse::<SessionId>()?,
            summary: Uuid::new_v4(),
            evidence: Evidence::quote(value),
            settled: false,
        },
        owner: Some(Owner::new("owner".to_owned())?),
        at: hour(at),
    })
}

fn fact<'a>(article: &'a EntityArticle, key: &str) -> TestResult<&'a Fact> {
    Ok(article
        .facts
        .iter()
        .find(|fact| fact.key == key)
        .ok_or_else(|| format!("no {key} fact"))?)
}

async fn seeded(api: &Api) -> TestResult<TopicRecord> {
    let host = entity(api, "web01", "Hosts").await?;
    let person = entity(api, "Ana", "People").await?;
    testkit::filing::capture(
        api.store.as_ref(),
        FIRST,
        "claude-code",
        "Set up web01",
        None,
    )
    .await?;
    api.store
        .assert_facts(&[
            said(&host, "os", "Ubuntu 22.04", FIRST, 0)?,
            said(&host, "address", "192.0.2.4", FIRST, 0)?,
            said(&person, "role", "maintainer", FIRST, 0)?,
            said(&host, "os", "Ubuntu 24.04", SECOND, 48)?,
            said(&host, "address", "192.0.2.9", SECOND, 50)?,
            said(&host, "address", "192.0.2.12", FIRST, 51)?,
        ])
        .await?;
    Ok(host)
}

#[tokio::test]
async fn entities_list_by_type_with_their_session_counts() -> TestResult {
    let api = Api::new(true)?;
    seeded(&api).await?;
    let all: Vec<EntitySummary> = api.json("GET", "/api/entities", "", StatusCode::OK).await?;
    let listed: Vec<(&str, &str, &str, usize)> = all
        .iter()
        .map(|summary| {
            (
                summary.title.as_str(),
                summary.entity_type.as_str(),
                summary.type_name.as_str(),
                summary.session_count,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("Ana", "people", "People", 1),
            ("web01", "hosts", "Hosts", 2)
        ]
    );
    let hosts: Vec<EntitySummary> = api
        .json("GET", "/api/entities?type=hosts", "", StatusCode::OK)
        .await?;
    assert_eq!(hosts.len(), 1);
    let old_token: Vec<EntitySummary> = api
        .json("GET", "/api/entities?type=machine", "", StatusCode::OK)
        .await?;
    assert_eq!(old_token, Vec::new());
    Ok(())
}

#[tokio::test]
async fn a_new_type_topic_is_listed_with_its_name_and_count() -> TestResult {
    let api = Api::new(true)?;
    let created: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"kind":"type","name":"Fleets"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let _north: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            &format!(
                r#"{{"kind":"entity","name":"North","parent":"{}"}}"#,
                created.id
            ),
            StatusCode::CREATED,
        )
        .await?;
    let listed: Vec<EntitySummary> = api
        .json("GET", "/api/entities?type=fleets", "", StatusCode::OK)
        .await?;
    assert_eq!(listed.len(), 1);
    let north = listed.first().ok_or("north")?;
    assert_eq!(north.title, "North");
    assert_eq!(north.entity_type, "fleets");
    assert_eq!(north.type_name, "Fleets");
    let types: Vec<EntityTypeInfo> = api
        .json("GET", "/api/entities/types", "", StatusCode::OK)
        .await?;
    let fleets = types
        .iter()
        .find(|row| row.key == "fleets")
        .ok_or("fleets")?;
    assert_eq!(fleets.type_name, "Fleets");
    assert_eq!(fleets.count, 1);
    let hosts = types.iter().find(|row| row.key == "hosts").ok_or("hosts")?;
    assert_eq!(hosts.count, 0);
    Ok(())
}

#[tokio::test]
async fn an_article_shows_facts_sources_conflicts_and_connections() -> TestResult {
    let api = Api::new(true)?;
    let host = seeded(&api).await?;
    let now: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    let os = fact(&now, "os")?;
    assert_eq!(os.value, "Ubuntu 24.04");
    assert_eq!(
        os.previous.as_ref().map(|previous| previous.value.as_str()),
        Some("Ubuntu 22.04")
    );
    let address = fact(&now, "address")?;
    let candidates: Vec<&str> = address
        .conflict
        .as_ref()
        .ok_or("two sessions disagree within a day")?
        .candidates
        .iter()
        .map(|candidate| candidate.value.as_str())
        .collect();
    assert_eq!(candidates, ["192.0.2.12", "192.0.2.9"]);
    let titles: Vec<&str> = address
        .sources
        .iter()
        .map(|source| source.session.title.as_str())
        .collect();
    assert_eq!(titles, ["Set up web01"]);
    let connected: Vec<(&str, usize)> = now
        .connected
        .iter()
        .map(|entity| (entity.title.as_str(), entity.shared_sessions))
        .collect();
    assert_eq!(connected, [("Ana", 1)]);
    let sessions: Vec<(&str, u64)> = now
        .sessions
        .iter()
        .map(|row| (row.title.as_str(), row.message_count))
        .collect();
    assert_eq!(
        sessions,
        [("Set up web01", 1)],
        "an uncaptured session has no row"
    );
    assert!(
        now.timeline
            .iter()
            .any(|entry| entry.text == "os: Ubuntu 24.04")
    );
    assert!(
        now.timeline
            .windows(2)
            .all(|pair| pair.first().map(|entry| entry.at) >= pair.last().map(|entry| entry.at)),
        "the timeline is newest first"
    );
    Ok(())
}

#[tokio::test]
async fn as_of_shows_the_entity_as_it_stood() -> TestResult {
    let api = Api::new(true)?;
    let host = seeded(&api).await?;
    let cut = hour(24).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let then: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}?asOf={cut}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(fact(&then, "os")?.value, "Ubuntu 22.04");
    assert!(fact(&then, "address")?.conflict.is_none());
    assert!(then.timeline.iter().all(|entry| entry.at <= hour(24)));
    Ok(())
}

async fn disputed(api: &Api) -> TestResult<(TopicRecord, String)> {
    let host = entity(api, "web01", "Hosts").await?;
    api.store
        .assert_facts(&[
            said(&host, "address", "192.0.2.4", FIRST, 0)?,
            said(&host, "address", "192.0.2.9", SECOND, 1)?,
        ])
        .await?;
    let path = format!("/api/entities/{}/facts", host.id);
    Ok((host, path))
}

#[tokio::test]
async fn your_fact_resolves_a_conflict_and_a_pin_outlasts_the_writer() -> TestResult {
    let api = Api::new(true)?;
    let (host, path) = disputed(&api).await?;
    let pinned: Fact = api
        .json(
            "PUT",
            &path,
            r#"{"key":"Address","value":"192.0.2.9","pinned":true}"#,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        (pinned.key.as_str(), pinned.value.as_str(), pinned.pinned),
        ("address", "192.0.2.9", true)
    );
    assert!(pinned.conflict.is_none(), "your fact resolves the conflict");
    api.store
        .assert_facts(&[NewClaim {
            at: Utc::now() + TimeDelta::hours(1),
            ..said(&host, "address", "192.0.2.77", FIRST, 0)?
        }])
        .await?;
    let shown: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    let address = fact(&shown, "address")?;
    assert_eq!(
        address.value, "192.0.2.9",
        "the writer never overwrites a pin"
    );
    assert_eq!(
        address
            .conflict
            .as_ref()
            .map(|conflict| conflict.candidates.len()),
        Some(1)
    );
    let unpinned: Fact = api
        .json(
            "PUT",
            &path,
            &format!(
                r#"{{"id":"{}","key":"address","value":"192.0.2.77","pinned":false}}"#,
                address.id
            ),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        (unpinned.id.as_str(), unpinned.pinned),
        (address.id.as_str(), false)
    );
    Ok(())
}

#[tokio::test]
async fn a_fact_edit_names_a_fact_of_its_entity_and_needs_a_value() -> TestResult {
    let api = Api::new(true)?;
    let (host, path) = disputed(&api).await?;
    let shown: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    let address = fact(&shown, "address")?;
    let other_key = format!(
        r#"{{"id":"{}","key":"os","value":"Debian 12","pinned":false}}"#,
        address.id
    );
    assert_eq!(
        api.error("PUT", &path, &other_key).await?.0,
        StatusCode::CONFLICT
    );
    let unknown = format!(
        r#"{{"id":"{}","key":"os","value":"Debian 12","pinned":false}}"#,
        Uuid::new_v4()
    );
    assert_eq!(
        api.error("PUT", &path, &unknown).await?.0,
        StatusCode::NOT_FOUND
    );
    let blank = r#"{"key":"os","value":"  ","pinned":false}"#;
    assert_eq!(
        api.error("PUT", &path, blank).await?.0,
        StatusCode::BAD_REQUEST
    );
    Ok(())
}

#[tokio::test]
async fn only_an_entity_has_an_article() -> TestResult {
    let api = Api::new(true)?;
    let work: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Mesh"}"#,
            StatusCode::CREATED,
        )
        .await?;
    for key in [
        work.id.clone(),
        Uuid::new_v4().to_string(),
        "mesh".to_owned(),
    ] {
        assert_eq!(
            api.error("GET", &format!("/api/entities/{key}"), "")
                .await?
                .0,
            StatusCode::NOT_FOUND,
            "{key}"
        );
    }
    Ok(())
}

struct ImportedFrom {
    inner: Arc<SqliteStore>,
    session: SessionId,
    provenance: Provenance,
}

#[async_trait]
impl FactStore for ImportedFrom {
    async fn assert_facts(&self, claims: &[NewClaim]) -> Result<(), FactError> {
        self.inner.assert_facts(claims).await
    }

    async fn claims(&self, entity: TopicId) -> Result<Vec<Claim>, FactError> {
        self.inner.claims(entity).await
    }

    async fn session_links(&self) -> Result<Vec<SessionLink>, FactError> {
        self.inner.session_links().await
    }

    async fn fact_sessions(&self, sessions: &[SessionId]) -> Result<Vec<FactSession>, FactError> {
        let mut found = self.inner.fact_sessions(sessions).await?;
        for session in &mut found {
            if session.session == self.session {
                session.provenance = Some(self.provenance.clone());
            }
        }
        Ok(found)
    }

    async fn entity_history(&self, entity: TopicId) -> Result<Vec<EntityHistory>, FactError> {
        self.inner.entity_history(entity).await
    }
}

#[tokio::test]
async fn entity_claims_name_the_importing_author() -> TestResult {
    let native = Api::new(true)?;
    let host = seeded(&native).await?;
    let (status, bytes) = native
        .call("GET", &format!("/api/entities/{}", host.id), "")
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!String::from_utf8(bytes)?.contains("importedFrom"));

    let provenance = Provenance {
        author: Some("phil".to_owned()),
        origin_date: None,
        verification: kernel::memory::Verification::Unreceived,
        checks: None,
    };
    let session: SessionId = FIRST.parse()?;
    let api = Api::over(true, |store| {
        Arc::new(ImportedFrom {
            inner: store,
            session,
            provenance,
        })
    })?;
    let host = seeded(&api).await?;
    let article: EntityArticle = api
        .json(
            "GET",
            &format!("/api/entities/{}", host.id),
            "",
            StatusCode::OK,
        )
        .await?;
    let address = fact(&article, "address")?;
    let source = address.sources.first().ok_or("the address has a source")?;
    assert_eq!(
        source.session.title,
        "Set up web01 (imported from phil, unverified)"
    );
    let from = source
        .imported_from
        .as_ref()
        .ok_or("the source is an import")?;
    assert_eq!(from.author.as_deref(), Some("phil"));
    assert_eq!(from.verification, ImportVerification::Unreceived);
    let rivals: Vec<bool> = address
        .conflict
        .as_ref()
        .ok_or("two sessions disagree within a day")?
        .candidates
        .iter()
        .map(|candidate| candidate.imported_from.is_some())
        .collect();
    assert_eq!(rivals, [true, false]);
    Ok(())
}

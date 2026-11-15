use crate::common;

use common::Scenario;
use common::ScenarioFile;
use common::Step;
use common::TestResult;
use common::expect_json;
use common::fresh_store;
use common::recording;
use kernel::memory::MemoryStore;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiGapsResponse;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiRedLink;
use kernel::wiki::WikiTopic;
use kernel::wiki::WikiTopicList;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use testkit::Json;

const INDEX: &str = "index: get, list, search, resolve, gate, redirects, gaps, marks";
#[tokio::test]
async fn wiki_reads_match_the_recording() -> TestResult {
    let file: ScenarioFile = recording("wiki.json")?;
    let scenario = file
        .scenarios
        .iter()
        .find(|scenario| scenario.name == INDEX)
        .ok_or("missing wiki index scenario")?;
    let fresh = fresh_store()?;
    seed_topics(&fresh.store, step(scenario, "listTopics")?).await?;
    seed_redirect(&fresh.store).await?;
    let mut ids = common::Ids::default();
    expect_list(
        &fresh.store,
        step(scenario, "listTopics")?,
        &mut ids,
        None,
        50,
        0,
    )
    .await?;
    expect_list(
        &fresh.store,
        step(scenario, "listTopics tag")?,
        &mut ids,
        Some("infra"),
        50,
        0,
    )
    .await?;
    expect_list(
        &fresh.store,
        step(scenario, "listTopics page")?,
        &mut ids,
        None,
        1,
        1,
    )
    .await?;
    expect_gaps(&fresh.store, step(scenario, "gaps")?, &mut ids).await?;
    match fresh
        .store
        .wiki_page(&WikiSlug::try_from("missing-topic".to_owned())?)
        .await?
    {
        WikiLookup::Missing => {}
        other @ (WikiLookup::Page(_) | WikiLookup::Redirect(_)) => {
            return Err(format!("missing topic lookup was {other:?}").into());
        }
    }
    match fresh
        .store
        .wiki_page(&WikiSlug::try_from("acmekit-rollout".to_owned())?)
        .await?
    {
        WikiLookup::Redirect(slug) => {
            let recorded = step(scenario, "followRedirect")?
                .result
                .as_ref()
                .ok_or("redirect")?;
            expect_json(recorded, &Json::String(slug), "followRedirect", &mut ids)?;
        }
        other @ (WikiLookup::Page(_) | WikiLookup::Missing) => {
            return Err(format!("redirect lookup was {other:?}").into());
        }
    }
    Ok(())
}

async fn expect_list(
    store: &SqliteStore,
    step: &Step,
    ids: &mut common::Ids,
    tag: Option<&str>,
    limit: u32,
    offset: u32,
) -> TestResult {
    let listed = store
        .wiki_topics(WikiFilter {
            query: None,
            tag: tag.map(str::to_owned),
            limit,
            offset,
        })
        .await?;
    let actual = serde_json::to_value(&listed)?;
    let recorded = step.result.as_ref().ok_or("list result")?;
    let expected = project_list(recorded)?;
    expect_json(&expected, &actual, &step.label, ids)
}

fn project_list(recorded: &Json) -> TestResult<Json> {
    let topics = recorded
        .get("topics")
        .and_then(Json::as_array)
        .ok_or("topics")?;
    let mut projected = Vec::new();
    for topic in topics {
        projected.push(project_topic(topic)?);
    }
    let total = recorded
        .get("total")
        .and_then(Json::as_u64)
        .ok_or("total")?;
    Ok(serde_json::to_value(WikiTopicList {
        topics: projected,
        total,
    })?)
}

fn project_topic(topic: &Json) -> TestResult<WikiTopic> {
    let state = json_text(topic, "currentState")?;
    let excerpt: String = state.chars().take(240).collect();
    Ok(WikiTopic {
        slug: json_text(topic, "slug")?,
        title: json_text(topic, "title")?,
        tags: string_list(topic, "tags")?,
        entities: string_list(topic, "entities")?,
        updated_at: common::instant(&json_text(topic, "updatedAt")?)?,
        excerpt,
        topic_id: None,
    })
}

fn string_list(topic: &Json, key: &str) -> TestResult<Vec<String>> {
    let Some(value) = topic.get(key).cloned() else {
        return Err(format!("missing {key}").into());
    };
    Ok(serde_json::from_value(value)?)
}

async fn expect_gaps(store: &SqliteStore, step: &Step, ids: &mut common::Ids) -> TestResult {
    let gaps = store.wiki_gaps(1).await?;
    let actual = serde_json::to_value(&gaps)?;
    let recorded = step.result.as_ref().ok_or("gaps")?;
    let expected = serde_json::to_value(projected_gaps(recorded)?)?;
    expect_json(&expected, &actual, "gaps", ids)
}

fn projected_gaps(recorded: &Json) -> TestResult<WikiGapsResponse> {
    let red_links = recorded.get("redLinks").cloned().ok_or("redLinks")?;
    let red_links: Vec<WikiRedLink> = serde_json::from_value(red_links)?;
    let stalest = recorded
        .get("stalest")
        .and_then(Json::as_array)
        .and_then(|rows| rows.first())
        .ok_or("stalest topic")?;
    Ok(WikiGapsResponse {
        red_links,
        stalest: vec![project_topic(stalest)?],
    })
}

async fn seed_topics(store: &SqliteStore, step: &Step) -> TestResult {
    let topics = step
        .result
        .as_ref()
        .and_then(|value| value.get("topics"))
        .and_then(Json::as_array)
        .ok_or("seed topics")?;
    for topic in topics {
        insert_topic(store, topic).await?;
    }
    Ok(())
}

async fn insert_topic(store: &SqliteStore, topic: &Json) -> TestResult {
    let slug = json_text(topic, "slug")?;
    let title = json_text(topic, "title")?;
    let state = json_text(topic, "currentState")?;
    let aliases = topic.get("aliases").cloned().ok_or("aliases")?.to_string();
    let tags = topic.get("tags").cloned().ok_or("tags")?.to_string();
    let entities = topic
        .get("entities")
        .cloned()
        .ok_or("entities")?
        .to_string();
    let related = topic.get("related").cloned().ok_or("related")?.to_string();
    let updated = json_text(topic, "updatedAt")?;
    let created = json_text(topic, "createdAt")?;
    let verified = topic
        .get("lastVerifiedAt")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let git_sha = topic
        .get("gitSha")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let search = format!("{title} {state}");
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_wiki_topics \
                    (slug, title, aliases, tags, entities, related, current_state, article, \
                     search_text, git_sha, last_verified_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?9, ?10, ?11, ?12)",
                params![
                    slug, title, aliases, tags, entities, related, state, search, git_sha,
                    verified, created, updated
                ],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn seed_redirect(store: &SqliteStore) -> TestResult {
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at) \
                 VALUES ('acmekit-rollout', 'acmekit-deploys', '2026-10-10T12:00:00.000Z')",
                [],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn json_text(value: &Json, key: &str) -> TestResult<String> {
    value
        .get(key)
        .and_then(Json::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {key}").into())
}

fn step<'a>(scenario: &'a Scenario, label: &str) -> TestResult<&'a Step> {
    scenario
        .steps
        .iter()
        .find(|step| step.label == label)
        .ok_or_else(|| format!("missing step {label}").into())
}

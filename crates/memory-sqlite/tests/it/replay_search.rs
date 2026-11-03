use crate::common;
#[path = "search_support/fts_seed.rs"]
mod seed;
use crate::search_support::superseded;
use seed::{seed_fts, seed_tag_pool};
use superseded::Superseded;

use std::str::FromStr as _;

use chrono::DateTime;
use chrono::Utc;
use common::Fresh;
use common::Ids;
use common::TestResult;
use common::batch;
use common::deploy_batch;
use common::expect_json;
use common::expect_logs;
use common::fresh_store;
use common::instant;
use common::message;
use kernel::memory::BrowseWindow;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::Limit;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::MemorySearchResult;
use kernel::memory::SearchMode;
use kernel::memory::SearchOutcome;
use kernel::memory::SearchQuery;
use kernel::memory::TagLiteral;
use kernel::memory::TagLiteralError;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use serde::Serialize;
use testkit::Json;
use wire::message::Role;

const CLOCK: &str = "2026-10-10T12:03:00.000Z";
const POOL_CLOCK: &str = "2026-10-10T12:00:00.000Z";
const LEAF_ID: &str = "00000000-0000-4000-8000-400000000001";
const BRANCH_ID: &str = "00000000-0000-4000-8000-400000000002";
const LEAF: &str = "The team decided to deploy the AcmeKit service with a blue-green rollout. The release branch is cut on Mondays, the deploy script runs from the release branch, and a health check on staging.example.com gates the traffic switch. Rollback is a traffic switch back to the previous colour.";
const BRANCH: &str = "Branch: staging deploys and database migration plans for AcmeKit";
const FILLER_RELEASE: &str = "zz0 zz1 zz2 zz3 zz4 zz5 zz6 zz7 zz8 zz9 zz10 zz11 zz12 zz13 zz14 zz15 zz16 zz17 zz18 zz19 zz20 zz21 zz22 zz23 zz24 zz25 zz26 zz27 zz28 zz29 release";
const TAGGED_NOTE: &str =
    "a long note that mentions the rollback only once among many other unrelated words here";

const SUPERSEDED: &[Superseded] = &[
    Superseded {
        query: "deploy release",
        hits: &[
            "run the deploy script from the release branch, then watch the health check",
            "",
            "how do we deploy the AcmeKit service to staging",
            FILLER_RELEASE,
            LEAF,
            BRANCH,
        ],
    },
    Superseded {
        query: "\"unbalanced AND (",
        hits: &[BRANCH, LEAF],
    },
];

const IN_SCOPE: &[&str] = &["runtime", "fts_only", "tag_pool_widening", "hybrid"];

const SKIPPED_SEARCH_SCENARIOS: &[(&str, &str)] = &[(
    "helpers",
    "buildFtsMatchQuery, relevanceFromBm25 and resolveTaskId are TypeScript internals, not a search scenario",
)];

#[test]
fn skipped_search_scenarios_are_named() -> TestResult {
    let file: Json = common::recording("search.json")?;
    let keys = file.as_object().ok_or("search.json is not an object")?;
    for key in keys.keys() {
        let named = IN_SCOPE.contains(&key.as_str())
            || SKIPPED_SEARCH_SCENARIOS
                .iter()
                .any(|(name, _reason)| *name == key);
        if !named {
            return Err(format!("search.json key {key} is neither replayed nor skipped").into());
        }
    }
    for (name, reason) in SKIPPED_SEARCH_SCENARIOS {
        if reason.is_empty() || keys.get(*name).is_none() {
            return Err(format!("skipped scenario {name} needs a reason and a recording").into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn fts_only_replay_matches_the_recording() -> TestResult {
    let file: Json = common::recording("search.json")?;
    let fts = file.get("fts_only").ok_or("fts_only")?;
    let fresh = seed_fts().await?;
    let mut ids = Ids::default();
    let clock = instant(field_str(fts, "clock")?)?;
    let notice = recorded_notice(fts)?;
    replay_memory(&fresh.store, fts, clock, notice, &mut ids).await?;
    replay_backend(&fresh.store, fts, clock, &mut ids).await?;
    expect_access(&fresh.store, fts, &mut ids).await?;
    let logs = string_list(fts.get("logs").ok_or("logs")?)?;
    expect_logs(&logs, &fresh.store.logs(), "fts_only logs", &mut ids)?;
    last_seven_days_keeps_the_october_rollback(&fresh.store, clock).await?;
    Ok(())
}

#[tokio::test]
async fn tag_pool_widening_returns_the_buried_hit() -> TestResult {
    let file: Json = common::recording("search.json")?;
    let pool = file.get("tag_pool_widening").ok_or("tag_pool_widening")?;
    let fresh = fresh_store()?;
    let now = instant(POOL_CLOCK)?;
    seed_tag_pool(&fresh.store, now).await?;
    let step = pool
        .get("steps")
        .and_then(Json::as_array)
        .and_then(|steps| steps.first())
        .ok_or("tag pool step")?;
    let result = step.get("result").ok_or("tag pool result")?;
    let query = asked("rollback", MemoryScope::Messages, 2);
    let mut query = query;
    query.tag = Some("project:rare".parse()?);
    let outcome = fresh.store.search_at(query, now, &Utc).await?;
    let mut ids = Ids::default();
    expect_degraded(
        result.get("degraded").ok_or("degraded")?,
        &outcome,
        &mut ids,
        "tag pool degraded",
    )?;
    expect_hits(
        result.get("results").ok_or("results")?,
        &outcome.results,
        &mut ids,
        "tag pool hits",
    )?;
    let logs = string_list(pool.get("logs").ok_or("logs")?)?;
    expect_logs(&logs, &fresh.store.logs(), "tag pool logs", &mut ids)?;
    Ok(())
}

#[tokio::test]
async fn search_modes_and_request_validation() -> TestResult {
    let fresh = fresh_store()?;
    let now = instant(CLOCK)?;
    for mode in [SearchMode::Fts, SearchMode::Hybrid, SearchMode::Vector] {
        let mut query = asked("deploy", MemoryScope::Both, 10);
        query.mode = mode;
        let outcome = fresh.store.search_at(query, now, &Utc).await?;
        assert!(outcome.results.is_empty(), "{mode}");
        assert!(outcome.degraded.is_some(), "{mode}");
        assert_eq!(outcome.expanded, []);
        assert!(outcome.fallback.is_none());
        assert!(outcome.synthesis.is_none());
    }
    fresh.store.arm_embed();
    let armed = fresh
        .store
        .search_at(asked("deploy", MemoryScope::Both, 10), now, &Utc)
        .await?;
    assert!(armed.degraded.is_some());
    for mode in [SearchMode::Trigram, SearchMode::Regex] {
        let mut query = asked("deploy", MemoryScope::Both, 10);
        query.mode = mode;
        let result = fresh.store.search_at(query, now, &Utc).await?;
        assert_eq!(result.results, []);
        assert!(result.degraded.is_none());
    }
    let mut synthesize = asked("deploy", MemoryScope::Both, 10);
    synthesize.synthesize = Some(true);
    let error = fresh
        .store
        .search_at(synthesize, now, &Utc)
        .await
        .expect_err("synthesis");
    assert_eq!(
        error.to_string(),
        "memory synthesis requires a configured summarizer endpoint"
    );
    let mut inverted = asked("deploy", MemoryScope::Both, 10);
    let stamp = now;
    inverted.since = Some(stamp);
    inverted.before = Some(stamp);
    let error = fresh
        .store
        .search_at(inverted, now, &Utc)
        .await
        .expect_err("range");
    assert_eq!(error.to_string(), "since must precede before");
    Ok(())
}

async fn replay_memory(
    store: &SqliteStore,
    fts: &Json,
    clock: DateTime<Utc>,
    notice: &Json,
    ids: &mut Ids,
) -> TestResult {
    let cases = array(fts, "memorySearch")?;
    for case in cases {
        let text = field_str(case, "query")?;
        if let Some(superseded) = superseded::find(SUPERSEDED, text) {
            expect_superseded(store, case, clock, ids, superseded).await?;
            continue;
        }
        let outcome = store
            .search_at(
                recorded_query(text, case.get("options").ok_or("options")?)?,
                clock,
                &Utc,
            )
            .await?;
        assert_slice(&outcome);
        expect_degraded(notice, &outcome, ids, &format!("memory {text} degraded"))?;
        expect_hits(
            case.get("hits").ok_or("hits")?,
            &outcome.results,
            ids,
            &format!("memory {text}"),
        )?;
    }
    Ok(())
}

async fn expect_superseded(
    store: &SqliteStore,
    case: &Json,
    clock: DateTime<Utc>,
    ids: &Ids,
    superseded: &Superseded,
) -> TestResult {
    let mut recorded = Vec::new();
    for hit in array(case, "hits")? {
        let id = field_str(hit, "id")?;
        recorded.push(if common::generated(id) {
            ids.actual(id)?
        } else {
            id.to_owned()
        });
    }
    superseded::reinforce(store, recorded, clock).await?;
    let fresh = seed_fts().await?;
    let query = recorded_query(superseded.query, case.get("options").ok_or("options")?)?;
    let outcome = fresh.store.search_at(query, clock, &Utc).await?;
    let contents: Vec<&str> = outcome
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(
        contents,
        superseded.hits,
        "{}: {}",
        superseded.query,
        superseded::OR_SUPERSEDES_AND
    );
    Ok(())
}

async fn replay_backend(
    store: &SqliteStore,
    fts: &Json,
    clock: DateTime<Utc>,
    ids: &mut Ids,
) -> TestResult {
    for case in array(fts, "backendSearch")? {
        let text = field_str(case, "query")?;
        let options = case.get("options").ok_or("options")?;
        if let Some(error) = case.get("error") {
            expect_rejected_tag(options, error)?;
            continue;
        }
        let response = case.get("response").ok_or("response")?;
        let outcome = store
            .search_at(recorded_query(text, options)?, clock, &Utc)
            .await?;
        assert_slice(&outcome);
        expect_degraded(
            response.get("degraded").ok_or("degraded")?,
            &outcome,
            ids,
            &format!("backend {text} degraded"),
        )?;
        expect_hits(
            response.get("results").ok_or("results")?,
            &outcome.results,
            ids,
            &format!("backend {text}"),
        )?;
    }
    Ok(())
}

fn expect_rejected_tag(options: &Json, error: &Json) -> TestResult {
    let raw = field_str(options, "tag")?;
    let recorded = field_str(error, "message")?;
    let Err(rejected) = TagLiteral::from_str(raw) else {
        return Err(format!("tag {raw} was accepted").into());
    };
    let message = rejected.to_string();
    match rejected {
        TagLiteralError::Filter { raw: got } if got == raw => {}
        TagLiteralError::Filter { raw: got } | TagLiteralError::Literal { raw: got } => {
            return Err(format!("tag error for {got}").into());
        }
    }
    if message.starts_with(recorded) {
        Ok(())
    } else {
        Err(format!("tag error {message} does not start with {recorded}").into())
    }
}

async fn last_seven_days_keeps_the_october_rollback(
    store: &SqliteStore,
    clock: DateTime<Utc>,
) -> TestResult {
    let mut query = asked("rollback", MemoryScope::Messages, 10);
    query.window = Some(BrowseWindow::Last7d);
    let outcome = store.search_at(query, clock, &Utc).await?;
    let contents: Vec<&str> = outcome
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(
        contents,
        ["the rollback switches traffic back to the previous colour"]
    );
    Ok(())
}

fn assert_slice(outcome: &SearchOutcome) {
    assert_eq!(outcome.expanded, []);
    assert!(outcome.fallback.is_none());
    assert!(outcome.synthesis.is_none());
}

fn recorded_query(text: &str, options: &Json) -> TestResult<SearchQuery> {
    let mut query = asked(text, scope_of(options)?, limit_of(options)?);
    if let Some(agent) = options.get("agent").and_then(Json::as_str) {
        query.agent = Some(agent.to_owned());
    }
    if let Some(raw) = options.get("tag").and_then(Json::as_str) {
        query.tag = Some(raw.parse()?);
    }
    Ok(query)
}

fn asked(text: &str, scope: MemoryScope, limit: u32) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        mode: SearchMode::Hybrid,
        scope,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(limit), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

fn scope_of(options: &Json) -> TestResult<MemoryScope> {
    match options.get("scope").and_then(Json::as_str) {
        None | Some("both") => Ok(MemoryScope::Both),
        Some("messages") => Ok(MemoryScope::Messages),
        Some("summaries") => Ok(MemoryScope::Summaries),
        Some(other) => Err(format!("scope {other}").into()),
    }
}

fn limit_of(options: &Json) -> TestResult<u32> {
    match options.get("limit").and_then(Json::as_u64) {
        None => Ok(10),
        Some(limit) => {
            u32::try_from(limit).map_err(|error| format!("limit {limit}: {error}").into())
        }
    }
}

fn expect_hits(
    recorded: &Json,
    actual: &[MemorySearchResult],
    ids: &mut Ids,
    at: &str,
) -> TestResult {
    let hits = recorded.as_array().ok_or("hits are not an array")?;
    let expected: Vec<MemorySearchResult> =
        hits.iter().map(project_hit).collect::<TestResult<_>>()?;
    let unrecorded: Vec<MemorySearchResult> = actual
        .iter()
        .cloned()
        .map(|hit| MemorySearchResult {
            session_id: None,
            ..hit
        })
        .collect();
    expect_json(
        &serde_json::to_value(expected)?,
        &codec(&unrecorded)?,
        at,
        ids,
    )
}

fn codec<T: Serialize + ?Sized>(value: &T) -> TestResult<Json> {
    Ok(serde_json::from_str(&serde_json::to_string(value)?)?)
}

fn project_hit(hit: &Json) -> TestResult<MemorySearchResult> {
    let source = match field_str(hit, "layer").or_else(|_| field_str(hit, "source"))? {
        "message" => MemoryHitSource::Message,
        "summary" => MemoryHitSource::Summary,
        other => return Err(format!("source {other}").into()),
    };
    let content = field_str(hit, "content")?.to_owned();
    let role = projected_role(hit, source, &content)?;
    let relevance_score = hit
        .get("relevanceScore")
        .or_else(|| hit.get("score"))
        .and_then(Json::as_f64)
        .ok_or("missing score")?;
    Ok(MemorySearchResult {
        chunk: None,
        id: field_str(hit, "id")?.to_owned(),
        source,
        content,
        role,
        agent: match hit.get("agent") {
            None | Some(Json::Null) => None,
            Some(Json::String(agent)) => Some(agent.clone()),
            Some(other) => return Err(format!("agent {other}").into()),
        },
        relevance_score,
        created_at: instant(field_str(hit, "createdAt")?)?,
        session_id: None,
        title: None,
    })
}

fn projected_role(hit: &Json, source: MemoryHitSource, content: &str) -> TestResult<Option<Role>> {
    let recorded = field_str(hit, "role")?;
    match source {
        MemoryHitSource::Message => Ok(Some(recorded.parse()?)),
        MemoryHitSource::Summary => {
            let kind_matches = match recorded {
                "leaf" => content.starts_with("The team decided"),
                "branch" => content.starts_with("Branch:"),
                _ => false,
            };
            if kind_matches {
                Ok(None)
            } else {
                Err(format!("summary role {recorded} does not match its content").into())
            }
        }
    }
}

fn expect_degraded(
    recorded: &Json,
    outcome: &SearchOutcome,
    ids: &mut Ids,
    at: &str,
) -> TestResult {
    expect_json(recorded, &serde_json::to_value(&outcome.degraded)?, at, ids)
}

async fn expect_access(store: &SqliteStore, fts: &Json, ids: &mut Ids) -> TestResult {
    let messages = access_table(
        store,
        "SELECT id, access_count, last_accessed_at FROM ros_messages ORDER BY rowid",
    )
    .await?;
    let summaries = access_table(
        store,
        "SELECT id, access_count, last_accessed_at FROM ros_summaries ORDER BY rowid",
    )
    .await?;
    expect_json(
        fts.get("access_after").ok_or("access_after")?,
        &messages,
        "access_after",
        ids,
    )?;
    expect_json(
        fts.get("summary_access_after")
            .ok_or("summary_access_after")?,
        &summaries,
        "summary_access_after",
        ids,
    )
}

async fn access_table(store: &SqliteStore, sql: &str) -> TestResult<Json> {
    let sql = sql.to_owned();
    let rows = store
        .with_conn(move |conn| {
            let mut statement = conn.prepare(&sql).map_err(|source| SqliteError::Query {
                operation: "access",
                source,
            })?;
            let loaded = statement
                .query_map([], |row| {
                    Ok(AccessRow {
                        id: row.get(0)?,
                        access_count: row.get(1)?,
                        last_accessed_at: row.get(2)?,
                    })
                })
                .map_err(|source| SqliteError::Query {
                    operation: "access",
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| SqliteError::Query {
                    operation: "access",
                    source,
                })?;
            Ok(loaded)
        })
        .await?;
    codec(&rows)
}

#[derive(Serialize)]
struct AccessRow {
    id: String,
    access_count: i64,
    last_accessed_at: Option<String>,
}

fn recorded_notice(fts: &Json) -> TestResult<&Json> {
    for case in array(fts, "backendSearch")? {
        if let Some(notice) = case
            .get("response")
            .and_then(|response| response.get("degraded"))
        {
            return Ok(notice);
        }
    }
    Err("backend search recorded no degraded notice".into())
}

fn array<'a>(value: &'a Json, key: &str) -> TestResult<&'a Vec<Json>> {
    value
        .get(key)
        .and_then(Json::as_array)
        .ok_or_else(|| format!("missing {key}").into())
}

fn field_str<'a>(value: &'a Json, key: &str) -> TestResult<&'a str> {
    value
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("missing {key}").into())
}

fn string_list(value: &Json) -> TestResult<Vec<String>> {
    value
        .as_array()
        .ok_or("expected a string array")?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "expected a string".into())
        })
        .collect()
}

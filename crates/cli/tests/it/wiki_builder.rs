#![cfg(test)]

use crate::wiki_support;

use kernel::model::FinishReason;
use kernel::queue::{FixedJob, JobName, JobQueue, WorkerRole};
use kernel::wiki::builder::{ExtractWiki, WIKI_PIPELINE_VERSION, WikiBuilderStore, WikiWork};
use memory_postgres::{PostgresMemory, PostgresQueue};
use sqlx::PgPool;
use testkit::fakes::FakeJobQueue;
use tokio_util::sync::CancellationToken;
use wiki_support::{TestResult, fixture, now, patch, provider, seed, seed_recording};
use wire::message::MessageContent;
use workers::wiki_builder::{
    WikiBuilderEnv, extract_wiki_job, handle_extract_wiki, handle_wiki_backfill,
};

const REPLAYED: &[&str] = &[
    "leaf once a full window exists",
    "leaves → branches → root with small batches",
    "truncated answer shrinks the leaf batch",
    "permanent failure → dead → rests an hour → revived",
    "below the floor until idle/stale",
];
const SKIPPED: &[(&str, &str)] = &[(
    "failing LLM fails the job; heartbeat never summarized",
    "the summarizer produces no summary in this scenario, so the wiki builder has no input",
)];

#[test]
fn wiki_summary_replay_accounts_for_every_scenario() {
    let fixture = fixture();
    assert_eq!(
        fixture.constants.wiki_pipeline_version,
        WIKI_PIPELINE_VERSION
    );
    for scenario in &fixture.scenarios {
        let replayed = REPLAYED
            .iter()
            .filter(|name| **name == scenario.name)
            .count();
        let skipped = SKIPPED
            .iter()
            .filter(|(name, reason)| *name == scenario.name && !reason.is_empty())
            .count();
        assert_eq!(replayed + skipped, 1, "{}", scenario.name);
    }
    assert_eq!(fixture.scenarios.len(), REPLAYED.len() + SKIPPED.len());
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_replays_every_recorded_summary_shape(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let cancel = CancellationToken::new();
    for scenario in fixture().scenarios {
        if SKIPPED.iter().any(|(name, _)| *name == scenario.name) {
            continue;
        }
        for row in scenario.tables.summaries.expect("replayed summaries") {
            seed_recording(&pool, &row).await?;
            let model = provider(&[patch()]);
            let queue = FakeJobQueue::default();
            let env = WikiBuilderEnv {
                store: &store,
                provider: &model,
                queue: &queue,
                model: "fake-chat",
                now: row.created_at,
                cancel: &cancel,
                topics: Some(wiki_support::ports(&store)),
            };
            let job = ExtractWiki {
                summary_id: row.id,
                conversation_id: Some(row.conversation_id),
            };
            wiki_support::filed(&store, &job).await?;
            let work = store.wiki_work(&job).await?;
            let WikiWork::Pending(summary) = work else {
                panic!("fresh extraction");
            };
            assert_eq!(summary.id, row.id);
            assert_eq!(summary.conversation_id, Some(row.conversation_id));
            assert_eq!(summary.content, row.content);
            let topics = handle_extract_wiki(&env, &extract_wiki_job(&job)?.payload).await?;
            assert_eq!(topics, vec!["acmekit-deploys"]);
            let calls = model.chat_requests().await;
            assert_eq!(calls.len(), 1);
            let MessageContent::Text(user) = &calls
                .first()
                .expect("request")
                .messages
                .get(1)
                .expect("user")
                .content
            else {
                panic!("text");
            };
            let sent: wire::JsonObject = serde_json::from_str(user)?;
            for (field, value) in wire::JsonObject::from_serialize(&summary)?.as_map() {
                assert_eq!(sent.get(field), Some(value), "{field}");
            }
            let state: (String, i32, Vec<String>) = sqlx::query_as("SELECT status, pipeline_version, topics_touched FROM rh_wiki_extractions WHERE summary_id = $1")
                .bind(row.id).fetch_one(&pool).await?;
            assert_eq!(state, ("done".to_owned(), WIKI_PIPELINE_VERSION, topics));
            sqlx::query("DELETE FROM rh_wiki_topics")
                .execute(&pool)
                .await?;
        }
        sqlx::query("DELETE FROM rh_summaries")
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM rh_conversations")
            .execute(&pool)
            .await?;
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_claims_jobs_persists_sources_and_retries_embedding_delivery(
    pool: PgPool,
) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let model = provider(&[patch()]);
    let cancel = CancellationToken::new();
    let job = seed(&pool).await?;
    let enqueued = extract_wiki_job(&job)?;
    assert_eq!(enqueued.key, Some(format!("wiki-ext-{}", job.summary_id)));
    queue.enqueue(enqueued).await?;
    let claimed = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::WikiBuilder),
        ))
        .await?
        .expect("wiki job");
    assert_eq!(claimed.name, JobName::Fixed(FixedJob::ExtractWiki));
    wiki_support::filed(&store, &job).await?;
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    let topics = handle_extract_wiki(&env, &claimed.payload).await?;
    assert_eq!(topics, vec!["acmekit-deploys"]);
    assert_eq!(handle_extract_wiki(&env, &claimed.payload).await?, topics);
    assert_eq!(model.chat_requests().await.len(), 1);
    queue.complete(claimed.id).await?;
    let embed = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Embedder),
        ))
        .await?
        .expect("embedding");
    assert_eq!(embed.name, JobName::Fixed(FixedJob::EmbedTarget));
    assert_eq!(
        workers::read_target(&embed.payload)?,
        workers::ParsedTarget::Ready(workers::EmbedTarget {
            table: workers::EmbedTable::WikiTopics,
            id: "acmekit-deploys".to_owned()
        })
    );
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_none()
    );
    let sha: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT t.git_sha, e.git_sha FROM rh_wiki_topics t JOIN rh_wiki_extractions e ON e.summary_id = $1 WHERE t.slug = 'acmekit-deploys'",
    ).bind(job.summary_id).fetch_one(&pool).await?;
    let output = tokio::process::Command::new("git")
        .current_dir(wiki.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .await?;
    assert!(output.status.success());
    let head = String::from_utf8(output.stdout)?.trim().to_owned();
    assert_eq!(sha, (Some(head.clone()), Some(head)));
    let markdown = tokio::fs::read_to_string(wiki.path().join("topics/acmekit-deploys.md")).await?;
    let page = kernel::wiki::page::parse_wiki_page(&markdown)?;
    assert_eq!(
        page.citations.first().ok_or("citation")?.note.as_deref(),
        Some("Blue-green adopted")
    );
    assert_eq!(
        page.meta.sources.first().ok_or("source")?.ids,
        vec![job.summary_id.to_string()]
    );
    let source: (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("SELECT source_id, conversation_id FROM rh_wiki_provenance")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        source,
        (job.summary_id, job.conversation_id.expect("conversation"))
    );
    let citation: (uuid::Uuid, Option<String>) =
        sqlx::query_as("SELECT summary_id, note FROM rh_wiki_citations")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        citation,
        (
            job.summary_id,
            Some("Decided to deploy blue-green.".to_owned())
        )
    );
    let redirects: (i64,) = sqlx::query_as("SELECT count(*) FROM rh_wiki_redirects")
        .fetch_one(&pool)
        .await?;
    assert_eq!(redirects.0, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pending_extractions_for_one_page_share_one_model_call(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let model = provider(&[patch()]);
    let cancel = CancellationToken::new();
    let mut jobs = Vec::new();
    for _ in 0..3 {
        let job = seed(&pool).await?;
        wiki_support::filed(&store, &job).await?;
        queue.enqueue(extract_wiki_job(&job)?).await?;
        jobs.push(job);
    }
    let claimed = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::WikiBuilder),
        ))
        .await?
        .expect("wiki job");
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    assert_eq!(
        handle_extract_wiki(&env, &claimed.payload).await?,
        vec!["acmekit-deploys"]
    );
    let calls = model.chat_requests().await;
    assert_eq!(calls.len(), 1);
    let MessageContent::Text(user) = &calls
        .first()
        .expect("request")
        .messages
        .get(1)
        .expect("user")
        .content
    else {
        panic!("text");
    };
    let sent: wire::JsonObject = serde_json::from_str(user)?;
    let summaries = sent
        .get("summaries")
        .and_then(|value| value.as_array())
        .map(Vec::len);
    assert_eq!(summaries, Some(3));
    let ids: Vec<uuid::Uuid> = jobs.iter().map(|job| job.summary_id).collect();
    let done: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM rh_wiki_extractions WHERE status = 'done' AND summary_id = ANY($1)),
                (SELECT count(DISTINCT summary_id) FROM rh_wiki_citations WHERE topic_slug = 'acmekit-deploys' AND summary_id = ANY($1))",
    )
    .bind(&ids)
    .fetch_one(&pool)
    .await?;
    assert_eq!(done, (3, 3));
    let left: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM graphile_worker._private_jobs WHERE key LIKE 'wiki-ext-%'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        left.0, 1,
        "only the lead job is left for its runner to complete"
    );
    let markdown = tokio::fs::read_to_string(wiki.path().join("topics/acmekit-deploys.md")).await?;
    let page = kernel::wiki::page::parse_wiki_page(&markdown)?;
    let mut sourced: Vec<String> = page
        .meta
        .sources
        .iter()
        .flat_map(|source| source.ids.clone())
        .collect();
    sourced.sort();
    let mut expected: Vec<String> = ids.iter().map(ToString::to_string).collect();
    expected.sort();
    assert_eq!(sourced, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_backfill_tops_up_missing_and_failed_leaves_and_never_reruns_done(
    pool: PgPool,
) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let model = provider(&[]);
    let cancel = CancellationToken::new();
    let mut expected = std::collections::BTreeSet::new();
    for (status, version, eligible) in [
        ("done", 3, false),
        ("done", 4, false),
        ("failed", 4, true),
        ("skipped", 3, false),
        ("done", 5, false),
    ] {
        let job = seed(&pool).await?;
        sqlx::query("INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, owner_user_id) VALUES ($1,$2,$3, 'owner')")
            .bind(job.summary_id).bind(status).bind(version).execute(&pool).await?;
        if eligible {
            expected.insert(job.summary_id);
        }
    }
    expected.insert(seed(&pool).await?.summary_id);
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    let one = kernel::wiki::builder::BackfillBudget::flat(1);
    assert_eq!(handle_wiki_backfill(&env, one).await?, 1);
    assert_eq!(
        handle_wiki_backfill(&env, one).await?,
        0,
        "the tier is full"
    );
    let all = kernel::wiki::builder::BackfillBudget::flat(100);
    assert_eq!(handle_wiki_backfill(&env, all).await?, expected.len() - 1);
    assert_eq!(
        handle_wiki_backfill(&env, all).await?,
        0,
        "queued leaves are not added twice"
    );
    let mut actual = std::collections::BTreeSet::new();
    while let Some(job) = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::WikiBuilder),
        ))
        .await?
    {
        let body: ExtractWiki = serde_json::from_value(serde_json::to_value(job.payload)?)?;
        assert!(actual.insert(body.summary_id));
        queue.complete(job.id).await?;
    }
    assert_eq!(actual, expected);
    assert_eq!(model.chat_requests().await.len(), 0);
    let crons = PostgresQueue::crontab(WorkerRole::WikiBuilder);
    assert!(crons.iter().any(
        |cron| cron.task == JobName::Fixed(FixedJob::EnqueueWikiBackfill)
            && cron.schedule == "*/1 * * * *"
    ));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn waiting_new_work_holds_the_backfill_to_its_busy_size(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let store = PostgresMemory::new(pool.clone());
    for _ in 0..3 {
        seed(&pool).await?;
    }
    let budget = kernel::wiki::builder::BackfillBudget { idle: 3, busy: 1 };
    let fresh = seed(&pool).await?;
    let mut new_work = extract_wiki_job(&fresh)?;
    new_work.priority = -10;
    queue.enqueue(new_work).await?;
    assert_eq!(store.wiki_backfill(budget).await?.len(), 1);
    sqlx::query("DELETE FROM graphile_worker._private_jobs")
        .execute(&pool)
        .await?;
    assert_eq!(store.wiki_backfill(budget).await?.len(), 3);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_extraction_constrains_the_model_and_reads_loose_slugs_and_dates(
    pool: PgPool,
) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let job = seed(&pool).await?;
    wiki_support::filed(&store, &job).await?;
    let answer = r#"[{"action":"create","slug":"AcmeKit/Deploys","title":"AcmeKit deploys","aliases":["","acmekit rollout"],"tags":["deploy"],"entities":[],"summary":"Deploys go blue-green.","article":null,"related":null,"history_entry":{"date":"2026-10-03T04:24:00Z","title":"Blue-green adopted","body":"Decided to deploy blue-green."},"type":null}]"#;
    let model = wiki_support::raw_provider(answer, FinishReason::Stop);
    let queue = FakeJobQueue::default();
    let cancel = CancellationToken::new();
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    let topics = handle_extract_wiki(&env, &extract_wiki_job(&job)?.payload).await?;
    assert_eq!(topics, vec!["acmekit-deploys"]);
    let calls = model.chat_requests().await;
    let request = calls.first().ok_or("one request")?;
    let MessageContent::Text(system) = &request.messages.first().ok_or("system")?.content else {
        return Err("a text system prompt".into());
    };
    assert!(system.contains(r#""pattern":"^[a-z0-9]+(-[a-z0-9]+)*$""#));
    assert!(system.contains(r#""format":"date""#));
    let constraint = request
        .options
        .json_schema
        .as_ref()
        .ok_or("a response schema")?;
    assert!(system.contains(&serde_json::to_string(constraint)?));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_wiki_writer_records_an_entity_s_facts_under_the_summary_s_session(
    pool: PgPool,
) -> TestResult {
    use kernel::facts::FactStore as _;
    use kernel::topics::TopicStore as _;
    use kernel::topics::{NewTopic, Slug, Topic, TopicId, TopicKey, TopicKind, TopicName};

    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let job = seed(&pool).await?;
    wiki_support::filed(&store, &job).await?;
    let hosts = store
        .find(&TopicKey::Slug(TopicKind::Type, Slug::fold("Hosts")?))
        .await?
        .ok_or("the Hosts type is seeded")?;
    let host = store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Entity,
                name: TopicName::new("acme-build")?,
                aliases: Vec::new(),
                parent: Some(hosts.id),
                directory: None,
                owner: Some(testkit::owner()?),
            },
            now(),
        )?)
        .await?;
    let answer = r#"[{"action":"update","slug":"acme-build","title":"acme-build","aliases":[],"tags":[],"entities":[],"summary":"The AcmeKit build box.","article":null,"related":null,"history_entry":null,"type":"Hosts","facts":[{"key":"OS","value":"Ubuntu 24.04","evidence":"acme-build was moved to Ubuntu 24.04"},{"key":"role","value":"","evidence":"unclear"}]}]"#;
    let model = wiki_support::raw_provider(answer, FinishReason::Stop);
    let queue = FakeJobQueue::default();
    let cancel = CancellationToken::new();
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    handle_extract_wiki(&env, &extract_wiki_job(&job)?.payload).await?;
    assert!(
        store.claims(host.id).await?.is_empty(),
        "the wiki builder does not file entity facts"
    );
    let calls = model.chat_requests().await;
    let request = calls.first().ok_or("one request")?;
    let MessageContent::Text(system) = &request.messages.first().ok_or("system")?.content else {
        return Err("a text system prompt".into());
    };
    assert!(system.contains("list in facts what this summary says is now true"));
    assert!(
        system.contains(r#""PatchFact""#),
        "the response schema carries facts"
    );
    handle_extract_wiki(&env, &extract_wiki_job(&job)?.payload).await?;
    assert!(
        store.claims(host.id).await?.is_empty(),
        "a second pass still files no entity claim"
    );
    Ok(())
}

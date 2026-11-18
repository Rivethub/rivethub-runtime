use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use kernel::memory::{
    CaptureBatchRequest, CaptureMessage, MemoryStore, SearchMode, SearchQuery, SummaryOwner,
};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName};
use kernel::tenancy::Scope;
use kernel::wiki::builder::{ExtractWiki, WikiBuilderStore, WikiRouting, WikiWork};
use kernel::wiki::entity_pass::{EntityStore, EntityWork, ExtractEntities};
use tokio::sync::{Barrier, Semaphore};
use workers::{
    EmbedSettings, LiveEnv, LiveRequest, SummarizeEnv, SummarySettings, WorkerError,
    handle_summarize,
};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn capture(
    store: &dyn MemoryStore,
    session: &str,
    rows: Vec<(String, chrono::DateTime<Utc>)>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let result = store
        .capture(
            CaptureBatchRequest {
                session_key: session.to_owned(),
                agent: "fixture".into(),
                channel: Some("tests".into()),
                title: Some("Ledger fixture".into()),
                settings: None,
                task_id: None,
                finalize: None,
                created_at: None,
                updated_at: None,
                messages: rows
                    .into_iter()
                    .map(|(content, at)| CaptureMessage {
                        event_id: Uuid::new_v4().to_string(),
                        role: Role::User,
                        content,
                        tool_name: None,
                        tool_args: None,
                        tool_result: None,
                        metadata: None,
                        created_at: Some(at),
                    })
                    .collect(),
            },
            &testkit::owner()?,
            None,
        )
        .await?;
    Ok(Uuid::parse_str(&result.conversation_id)?)
}

async fn settle(
    store: &dyn MemoryStore,
    session: &str,
) -> Result<LiveRequest, Box<dyn std::error::Error>> {
    let state = store
        .mark_summary_settled(session, &testkit::owner()?)
        .await?
        .ok_or("settlement")?;
    Ok(LiveRequest {
        conversation_id: state.conversation_id,
        revision: Some(state.input_revision),
    })
}

async fn run(
    store: &dyn SummaryStore,
    script: &Scripted,
    request: &LiveRequest,
    attempt: u32,
    context: u32,
    permits: &Semaphore,
) -> Result<(), WorkerError> {
    let queue = testkit::fakes::FakeJobQueue::default();
    let mut settings = SummarySettings::standard("invented-model")?;
    settings.context_tokens = context;
    let cancel = CancellationToken::new();
    let summary = SummarizeEnv {
        store,
        provider: script,
        queue: &queue,
        settings: &settings,
        now: Utc::now(),
        cancel: &cancel,
    };
    handle_summarize(
        &LiveEnv {
            summary: &summary,
            embedder: script,
            embed: EmbedSettings::standard(),
            permits,
        },
        request,
        attempt,
    )
    .await
}

fn short(text: &str) -> Vec<(String, chrono::DateTime<Utc>)> {
    vec![(text.to_owned(), Utc::now())]
}

async fn short_and_reopen(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let session = "ledger-short";
    let id = capture(store, session, short("Record the olive receipt number.")).await?;
    let state = store.summary_state(id).await?.ok_or("active state")?;
    let mut request = LiveRequest {
        conversation_id: id,
        revision: Some(state.input_revision),
    };
    let script = Scripted::live();
    let permits = Semaphore::new(3);
    run(store, &script, &request, 1, 131_072, &permits).await?;
    assert!(script.calls.lock().await.is_empty());
    request = settle(store, session).await?;
    script.truncated.store(true, SeqCst);
    run(store, &script, &request, 1, 131_072, &permits).await?;
    let first = store
        .live_input(id)
        .await?
        .and_then(|input| input.expected_root)
        .ok_or("session ID")?;
    let row = store
        .summary_node(&SummaryOwner::Conversation(id), &first.to_string())
        .await?
        .ok_or("summary")?;
    assert_eq!(row.quality_flags & 1, 1);
    assert_eq!(script.calls.lock().await.len(), 1);
    assert_eq!(row.vector, kernel::memory::VectorState::Embedded);

    capture(store, session, short("The amber receipt is still open.")).await?;
    request = settle(store, session).await?;
    script.truncated.store(false, SeqCst);
    script.fail_embeddings.store(true, SeqCst);
    assert!(matches!(
        run(store, &script, &request, 1, 131_072, &permits).await,
        Err(WorkerError::Embed(_))
    ));
    assert_eq!(
        store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root),
        Some(first)
    );
    assert_eq!(script.calls.lock().await.len(), 2);
    script.fail_embeddings.store(false, SeqCst);
    run(store, &script, &request, 2, 131_072, &Semaphore::new(3)).await?;
    assert_eq!(script.calls.lock().await.len(), 2);
    assert_ne!(
        store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root),
        Some(first)
    );
    let state = store.summary_state(id).await?.ok_or("published state")?;
    assert_eq!(state.summarized_through, request.revision);
    Ok(())
}

async fn parts_and_late_input(
    store: &(impl SummaryStore + MemoryStore + WikiBuilderStore),
) -> TestResult {
    let session = "ledger-parts";
    let base = Utc::now() - chrono::TimeDelta::days(1);
    let rows = (1..=8)
        .map(|index| {
            (
                format!("Deposit {index} {}", "chestnut ".repeat(1_750)),
                base + chrono::TimeDelta::minutes(index),
            )
        })
        .collect();
    let id = capture(store, session, rows).await?;
    let request = settle(store, session).await?;
    let script = Scripted {
        part_pause: Some(Arc::new(Barrier::new(3))),
        ..Scripted::live()
    };
    script.fail_part_once.store(true, SeqCst);
    script.large_parts.store(true, SeqCst);
    let permits = Semaphore::new(3);
    let failed = tokio::time::timeout(
        Duration::from_secs(10),
        run(store, &script, &request, 1, 32_768, &permits),
    )
    .await?;
    assert!(matches!(failed, Err(WorkerError::Llm { .. })), "{failed:?}");
    assert_eq!(script.peak.load(SeqCst), 3);
    assert_eq!(script.calls.lock().await.len(), 4);
    assert!(
        store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root)
            .is_none()
    );

    run(store, &script, &request, 2, 32_768, &Semaphore::new(3)).await?;
    let input = store.live_input(id).await?.ok_or("first publication")?;
    assert_eq!(input.parts.len(), 4);
    let calls = script.calls.lock().await;
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.kind == "TOP_DOWN_PART")
            .count(),
        5
    );
    assert!(
        calls
            .iter()
            .filter(|call| call.kind == "TOP_DOWN_CARRY")
            .count()
            >= 4
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.kind == "TOP_DOWN_FINAL")
            .count(),
        1
    );
    drop(calls);

    for part in &input.parts {
        complete_wiki(store, id, part.id, part.sources.len()).await?;
    }

    capture(
        store,
        session,
        vec![("The late coral receipt arrived.".into(), base)],
    )
    .await?;
    let next = settle(store, session).await?;
    script.large_parts.store(false, SeqCst);
    run(store, &script, &next, 1, 32_768, &Semaphore::new(3)).await?;
    let next_input = store.live_input(id).await?.ok_or("second publication")?;
    assert_eq!(next_input.parts.len(), 5);
    for old in input.parts {
        let kept = next_input
            .parts
            .iter()
            .find(|part| part.id == old.id)
            .ok_or("retained part")?;
        assert_eq!(kept.content, old.content);
        assert_eq!(kept.sources, old.sources);
        assert_eq!(kept.ordinal, old.ordinal);
        assert_eq!(kept.input_key, old.input_key);
    }
    let delta = next_input
        .parts
        .iter()
        .find(|part| part.issued_part_ordinal == Some(5))
        .ok_or("new part")?;
    complete_wiki(store, id, delta.id, 1).await?;
    for kept in &next_input.parts {
        assert!(matches!(
            store
                .wiki_work(&ExtractWiki {
                    summary_id: kept.id,
                    conversation_id: Some(id)
                })
                .await?,
            WikiWork::Complete { .. }
        ));
    }

    let calls = script.calls.lock().await;
    let new_parts: Vec<_> = calls
        .iter()
        .filter(|call| call.kind == "TOP_DOWN_PART")
        .collect();
    assert_eq!(new_parts.len(), 6);
    assert!(
        new_parts
            .last()
            .ok_or("delta part")?
            .user
            .contains("[m00001")
    );
    assert_eq!(
        next_input.messages.first().ok_or("late message")?.content,
        "The late coral receipt arrived."
    );
    Ok(())
}

async fn superseded_and_exhaustion(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let session = "ledger-pause";
    let base = Utc::now() - chrono::TimeDelta::hours(1);
    let id = capture(
        store,
        session,
        (1..=6)
            .map(|i| {
                (
                    format!("Batch {i} {}", "chestnut ".repeat(1_750)),
                    base + chrono::TimeDelta::minutes(i),
                )
            })
            .collect(),
    )
    .await?;
    let request = settle(store, session).await?;
    let barrier = Arc::new(Barrier::new(4));
    let script = Scripted {
        part_pause: Some(barrier.clone()),
        ..Scripted::live()
    };
    let permits = Semaphore::new(3);
    let interruption = async {
        loop {
            if script.calls.lock().await.len() == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        capture(store, session, short("Capture resumed for a final note.")).await?;
        barrier.wait().await;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, interrupted) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            run(store, &script, &request, 1, 32_768, &permits),
            interruption
        )
    })
    .await?;
    interrupted?;
    result?;
    let workspace = store.live_run(id).await?.ok_or("saved workspace")?;
    assert_eq!(Some(workspace.revision), request.revision);
    assert!(
        store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root)
            .is_none()
    );

    let script = Scripted::live();
    let next = settle(store, session).await?;
    script.empty.store(true, SeqCst);
    assert!(matches!(
        run(store, &script, &next, 3, 32_768, &permits).await,
        Err(WorkerError::EmptySummary)
    ));
    assert_eq!(
        script.calls.lock().await.len(),
        1,
        "the two unchanged part checkpoints survived"
    );
    let failed = store.summary_state(id).await?.ok_or("failed state")?;
    assert_eq!(failed.failed_revision, next.revision);
    assert!(failed.summarized_through.is_none());

    capture(store, session, short("One more raw observation.")).await?;
    assert!(
        store
            .summary_state(id)
            .await?
            .ok_or("new revision")?
            .failed_revision
            .is_none()
    );
    let latest = settle(store, session).await?;
    script.empty.store(false, SeqCst);
    run(store, &script, &latest, 1, 32_768, &permits).await?;
    assert_eq!(
        store
            .summary_state(id)
            .await?
            .ok_or("published state")?
            .summarized_through,
        latest.revision
    );
    Ok(())
}

async fn no_rendered_messages(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let owner = testkit::owner()?;
    let now = Utc::now();
    let id = capture(store, "ledger-empty", short("")).await?;
    let request = settle(store, "ledger-empty").await?;
    let script = Scripted::live();
    run(store, &script, &request, 1, 131_072, &Semaphore::new(3)).await?;
    assert!(script.calls.lock().await.is_empty());
    assert!(
        store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root)
            .is_none()
    );
    let settled = store
        .mark_summary_settled("ledger-empty", &owner)
        .await?
        .ok_or("settled empty")?;
    assert_eq!(
        settled.status,
        kernel::memory::settlement::SettleStatus::AlreadySummarized
    );
    assert!(
        settled
            .settled_at
            .is_some_and(|at| at >= now - chrono::TimeDelta::seconds(1))
    );
    Ok(())
}

#[tokio::test]
async fn top_down_live_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?.with_summary_index_key([7; 32]);
    short_and_reopen(&store).await?;
    parts_and_late_input(&store).await?;
    superseded_and_exhaustion(&store).await?;
    no_rendered_messages(&store).await?;
    contend(&store, false).await?;
    let id = first_published(&store).await?;
    store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_summaries SET index_needs_repair = 1, embedding = NULL WHERE id = ?1",
                [id.to_string()],
            )
            .map_err(memory_sqlite::SqliteError::Pragma)?;
            Ok(())
        })
        .await?;
    repair_job(Arc::new(store.clone()), Arc::new(store), id).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_live_postgres(pool: sqlx::PgPool) -> TestResult {
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let wiki = tempfile::tempdir()?;
    let store = Arc::new(
        memory_postgres::PostgresMemory::new(scoped)
            .with_summary_index_key([7; 32])
            .with_wiki_directory(wiki.path().to_path_buf())
            .with_wiki_owner(testkit::owner()?),
    );
    Scope::Maintenance
        .run(async {
            short_and_reopen(store.as_ref()).await?;
            parts_and_late_input(store.as_ref()).await?;
            superseded_and_exhaustion(store.as_ref()).await?;
            no_rendered_messages(store.as_ref()).await?;
            contend(store.as_ref(), true).await?;
            entity_once(store.as_ref()).await?;
            let id = first_published(store.as_ref()).await?;
            sqlx::query("UPDATE rh_summaries SET index_needs_repair = true WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await?;
            let queue = Arc::new(store.queue().await?);
            repair_job(queue, store.clone(), id).await
        })
        .await?;
    let passage: (chrono::DateTime<Utc>, chrono::DateTime<Utc>) = sqlx::query_as(
        "SELECT p.earliest_at, p.latest_at FROM rh_summary_passages p JOIN rh_summaries s ON s.id = p.record_id
         JOIN rh_conversations c ON c.id = s.conversation_id WHERE c.session_key = 'ledger-short' AND s.kind = 'session'"
    ).fetch_one(&pool).await?;
    assert!(passage.0 <= passage.1);
    let hits = Scope::Maintenance
        .run(store.search(SearchQuery {
            text: "saffron".into(),
            mode: SearchMode::Fts,
            scope: kernel::memory::MemoryScope::Summaries,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: kernel::memory::Limit::clamped(None, 20, 1..=100),
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
        }))
        .await?;
    assert!(hits.results.iter().any(|hit| hit.passage.is_some()));
    store.close().await?;
    Ok(())
}

async fn complete_wiki(
    store: &dyn WikiBuilderStore,
    conversation: Uuid,
    id: Uuid,
    count: usize,
) -> TestResult {
    let job = ExtractWiki {
        summary_id: id,
        conversation_id: Some(conversation),
    };
    let WikiWork::Pending(input) = store.wiki_work(&job).await? else {
        return Err("pending wiki anchor".into());
    };
    let pending = input.delivery.as_ref().ok_or("wiki delivery")?;
    assert_eq!(pending.pending.len(), count);
    store
        .apply_wiki(
            &job,
            Vec::new(),
            &WikiRouting {
                input: Some(input),
                ..WikiRouting::default()
            },
            Utc::now(),
        )
        .await?;
    assert!(matches!(
        store.wiki_work(&job).await?,
        WikiWork::Complete { .. }
    ));
    Ok(())
}

async fn entity_once(store: &(impl EntityStore + MemoryStore + SummaryStore)) -> TestResult {
    let session = "ledger-entity";
    let now = Utc::now();
    let id = capture(
        store,
        session,
        vec![
            (
                "The turquoise tablet is first.".into(),
                now - chrono::TimeDelta::minutes(1),
            ),
            ("The silver tablet is second.".into(), now),
        ],
    )
    .await?;
    let script = Scripted::live();
    let permits = Semaphore::new(3);
    for expected in [
        vec![
            "The turquoise tablet is first.",
            "The silver tablet is second.",
        ],
        vec!["The violet tablet arrived later."],
    ] {
        let request = settle(store, session).await?;
        run(store, &script, &request, 1, 131_072, &permits).await?;
        let root = store
            .live_input(id)
            .await?
            .and_then(|input| input.expected_root)
            .ok_or("entity anchor")?;
        let job = ExtractEntities {
            summary_id: root,
            conversation_id: Some(id),
            session_at: None,
        };
        let EntityWork::Pending(anchor) = store.entity_work(&job).await? else {
            return Err("pending entity anchor".into());
        };
        let rows = store.transcript(std::slice::from_ref(&anchor)).await?;
        assert_eq!(
            rows.iter()
                .map(|row| row.content.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        store
            .finish_entity_delivery(&[anchor], &[], None, Utc::now())
            .await?;
        assert!(matches!(
            store.entity_work(&job).await?,
            EntityWork::Complete { .. }
        ));
        if expected.len() == 2 {
            capture(store, session, short("The violet tablet arrived later.")).await?;
        }
    }
    Ok(())
}

async fn contend(store: &(impl SummaryStore + MemoryStore), separate_workers: bool) -> TestResult {
    let rows = |name: &str| {
        (1..=6)
            .map(|i| {
                (
                    format!("{name} deposit {i} {}", "juniper ".repeat(1_975)),
                    Utc::now() + chrono::TimeDelta::milliseconds(i),
                )
            })
            .collect()
    };
    capture(store, "permit-alpha", rows("alpha")).await?;
    capture(store, "permit-beta", rows("beta")).await?;
    let (alpha, beta) = (
        settle(store, "permit-alpha").await?,
        settle(store, "permit-beta").await?,
    );
    let script = Scripted::live();
    let worker = Semaphore::new(3);
    let other_worker = Semaphore::new(3);
    let second = if separate_workers {
        &other_worker
    } else {
        &worker
    };
    let (first, second) = tokio::join!(
        run(store, &script, &alpha, 1, 32_768, &worker),
        run(store, &script, &beta, 1, 32_768, second),
    );
    first?;
    second?;
    assert_eq!(script.part_calls.load(SeqCst), 6);
    assert!(script.peak.load(SeqCst) <= 3);
    Ok(())
}

async fn first_published(
    store: &(impl SummaryStore + MemoryStore),
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let script = Scripted::live();
    let conversation = capture(
        store,
        "repair-ledger",
        short("Record the pending acorn test."),
    )
    .await?;
    let request = settle(store, "repair-ledger").await?;
    run(store, &script, &request, 1, 131_072, &Semaphore::new(3)).await?;
    Ok(store
        .live_input(conversation)
        .await?
        .and_then(|input| input.expected_root)
        .ok_or("repairable root")?)
}

async fn repair_job<Q, S>(queue: Arc<Q>, store: Arc<S>, id: Uuid) -> TestResult
where
    Q: JobQueue + 'static,
    S: SummaryStore + kernel::memory::EmbeddingStore + 'static,
{
    let script = Arc::new(Scripted::live());
    assert!(store.repair_input(id).await?.is_some());
    let target = kernel::memory::EmbedTarget {
        id: id.to_string(),
        table: kernel::memory::EmbedTable::Summaries,
    };
    let queued = queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::EmbedTarget),
            payload: wire::JsonObject::from_serialize(
                &serde_json::json!({ "targetTable": "ros_summaries", "targetId": id }),
            )?,
            key: Some(workers::embed_key(&target)),
            key_mode: JobKeyMode::KeepRunAt,
            priority: -30,
            run_at: None,
            max_attempts: Some(2),
        })
        .await?;
    let mut deps = crate::common::deps(store.clone(), script.clone());
    deps.summaries = store.clone();
    deps.embed = EmbedSettings::standard();
    let runner = workers::Runner::new(
        queue.clone(),
        workers::RunnerConfig::new(workers::ProcessRole::Embedder, 1)?,
        deps,
    );
    let cancel = CancellationToken::new();
    let work = async {
        let mut idle = runner.idle();
        idle.wait_for(|idle| *idle).await.map(drop)?;
        cancel.cancel();
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, idle) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(runner.run(cancel.clone()), work)
    })
    .await?;
    result?;
    idle?;
    assert!(queue.read(queued).await?.is_none());
    assert!(store.repair_input(id).await?.is_none());
    assert!(script.calls.lock().await.is_empty());
    Ok(())
}

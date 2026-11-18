use kernel::memory::summary_conversion::{
    ConversionCharge, ConversionControl, ConversionLease, ConversionOutcome, ConversionUsage,
    LEASE, SummaryConversion,
};
use kernel::memory::{NewSummary, SummaryKind, SummarySource};
use sha2::{Digest as _, Sha256};
use tokio::sync::Notify;
use wire::time::TimestampFormat as _;
use workers::handle_conversion_tick;

use super::*;

const QUIET: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

struct Provider {
    script: Scripted,
    chat_gate: std::sync::atomic::AtomicBool,
    embed_gate: std::sync::atomic::AtomicBool,
    embed_calls: std::sync::atomic::AtomicUsize,
    entered: Notify,
    proceed: Notify,
}

impl Provider {
    fn new() -> Self {
        Self {
            script: Scripted::live(),
            chat_gate: true.into(),
            embed_gate: false.into(),
            embed_calls: 0.into(),
            entered: Notify::new(),
            proceed: Notify::new(),
        }
    }
}

#[async_trait]
impl ModelProvider for Provider {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let block = self.chat_gate.swap(false, SeqCst);
        let mut reply = self.script.chat(request).await?;
        if block {
            self.entered.notify_one();
            self.proceed.notified().await;
        }
        reply.usage = Some(wire::stream::LlmUsage {
            prompt_tokens: 400,
            completion_tokens: 22,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
        });
        Ok(reply)
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        self.embed_calls.fetch_add(1, SeqCst);
        if self.embed_gate.swap(false, SeqCst) {
            self.entered.notify_one();
            self.proceed.notified().await;
        }
        self.script.embed(texts).await
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        self.script.stream(request).await
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        self.script.list_models().await
    }
}

async fn tick(
    store: &dyn SummaryStore,
    provider: &Provider,
    permits: &Semaphore,
) -> Result<(), WorkerError> {
    let queue = testkit::fakes::FakeJobQueue::default();
    let cancel = CancellationToken::new();
    let mut settings = SummarySettings::standard("conversion-fixture")?;
    settings.context_tokens = 32_768;
    let summary = SummarizeEnv {
        store,
        provider,
        queue: &queue,
        settings: &settings,
        now: Timestamp::now(),
        cancel: &cancel,
    };
    handle_conversion_tick(
        &LiveEnv {
            summary: &summary,
            embedder: provider,
            embed: EmbedSettings::standard(),
            permits,
        },
        store.summary_conversion().ok_or(WorkerError::Closed)?,
    )
    .await
}

async fn legacy(store: &(impl SummaryStore + MemoryStore)) -> TestResult<(Uuid, Uuid)> {
    let at = Timestamp::now() - wire::time::elapsed_days(3);
    let id = capture(
        store,
        "historical-conversion",
        vec![("Store the amaranth log.".into(), at)],
    )
    .await?;
    let input = store.live_input(id).await?.ok_or("legacy source")?;
    let message = input
        .messages
        .first()
        .and_then(|row| row.id.as_ref())
        .ok_or("legacy ID")?
        .parse()?;
    let old = Uuid::new_v4();
    store
        .write_summary(NewSummary {
            id: old,
            conversation_id: id,
            kind: SummaryKind::Leaf,
            depth: 0,
            content: "The old amaranth log.".into(),
            message_count: 1,
            earliest_at: at,
            latest_at: at,
            model: "fixture".into(),
            pipeline_version: 6,
            created_at: at,
            sources: vec![SummarySource {
                message_id: message,
                ordinal: 0,
            }],
            child_ids: Vec::new(),
        })
        .await?;
    Ok((id, old))
}

async fn conversion(
    store: &(impl SummaryStore + MemoryStore),
    historical: Uuid,
    old: Uuid,
    expire: impl std::future::Future<Output = TestResult>,
) -> TestResult {
    let provider = Provider::new();
    let permits = Semaphore::new(3);
    let capability = store.summary_conversion().ok_or("conversion support")?;
    let before = store
        .live_input(historical)
        .await?
        .ok_or("legacy input")?
        .messages;
    assert!(!capability.status().await?.enabled);
    tick(store, &provider, &permits).await?;
    assert!(provider.script.calls.lock().await.is_empty());
    capability
        .control(ConversionControl::Start {
            rate: None,
            include_giants: false,
        })
        .await?;
    tick(store, &provider, &permits).await?;
    assert!(capability.status().await?.inventory_complete);

    verify_foreground_priority(store, capability, historical, old, &provider, &permits).await?;
    provider.script.fail_embeddings.store(true, SeqCst);
    tick(store, &provider, &permits).await?;
    assert_eq!(
        capability.status().await?.recent_failures,
        [(historical, "summary embedding failed".into())]
    );
    tick(store, &provider, &permits).await?;
    assert_eq!(
        provider.embed_calls.load(SeqCst),
        1,
        "failed items are skipped"
    );
    capability
        .control(ConversionControl::Retry {
            conversation: historical,
        })
        .await?;
    provider.script.fail_embeddings.store(false, SeqCst);

    recover_conversion(
        store,
        capability,
        historical,
        old,
        &provider,
        (&permits, expire),
    )
    .await?;
    let after = store
        .live_input(historical)
        .await?
        .ok_or("converted tree")?;
    assert!(after.expected_root.is_some());
    assert!(!after.expected_ids.contains(&old));
    assert_eq!(
        Sha256::digest(serde_json::to_vec(&before)?),
        Sha256::digest(serde_json::to_vec(&after.messages)?)
    );
    let state = store
        .summary_state(historical)
        .await?
        .ok_or("published revision")?;
    assert_eq!(state.summarized_through, Some(state.input_revision));
    Ok(())
}

async fn verify_foreground_priority(
    store: &(impl SummaryStore + MemoryStore),
    capability: &dyn SummaryConversion,
    historical: Uuid,
    old: Uuid,
    provider: &Provider,
    permits: &Semaphore,
) -> TestResult {
    let interruption = async {
        provider.entered.notified().await;
        assert_eq!(capability.status().await?.current, Some(historical));
        assert!(capability.claim(QUIET).await?.is_none());
        assert!(
            store
                .live_input(historical)
                .await?
                .ok_or("old tree")?
                .expected_ids
                .contains(&old)
        );
        let live_session = "conversion-foreground";
        capture(
            store,
            live_session,
            short("File the fresh periwinkle ledger."),
        )
        .await?;
        let live = settle(store, live_session).await?;
        assert!(capability.claim(QUIET).await?.is_none());
        provider.proceed.notify_one();
        Ok::<_, Box<dyn std::error::Error>>(live)
    };
    let (result, live) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(tick(store, provider, permits), interruption)
    })
    .await?;
    result?;
    let live = live?;
    assert_eq!(provider.script.calls.lock().await.len(), 1);
    assert_eq!(
        provider.embed_calls.load(SeqCst),
        0,
        "conversion yielded before its next request"
    );
    tick(store, provider, permits).await?;
    assert_eq!(
        provider.script.calls.lock().await.len(),
        1,
        "ready foreground blocks admission"
    );
    run(store, &provider.script, &live, 1, 32_768, permits).await?;
    assert_eq!(provider.script.calls.lock().await.len(), 2);

    Ok(())
}

async fn recover_conversion(
    store: &(impl SummaryStore + MemoryStore),
    capability: &dyn SummaryConversion,
    historical: Uuid,
    old: Uuid,
    provider: &Provider,
    (permits, expire): (&Semaphore, impl std::future::Future<Output = TestResult>),
) -> TestResult {
    provider.embed_gate.store(true, SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            () = provider.entered.notified() => Ok(()),
            result = tick(store, provider, permits) => Err(format!("crash fixture finished early: {result:?}")),
        }
    }).await??;
    assert_eq!(capability.status().await?.current, Some(historical));
    expire.await?;

    provider.embed_gate.store(true, SeqCst);
    let pause_before_publish = async {
        provider.entered.notified().await;
        capability.control(ConversionControl::Pause).await?;
        provider.proceed.notify_one();
        Ok::<_, kernel::memory::SummaryStoreError>(())
    };
    let (result, paused) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(tick(store, provider, permits), pause_before_publish)
    })
    .await?;
    result?;
    paused?;
    assert!(
        store
            .live_input(historical)
            .await?
            .ok_or("paused tree")?
            .expected_ids
            .contains(&old)
    );
    assert_eq!(capability.status().await?.complete, 0);
    capability
        .control(ConversionControl::Resume {
            include_giants: false,
        })
        .await?;
    tick(store, provider, permits).await?;
    let status = capability.status().await?;
    assert_eq!(
        provider.embed_calls.load(SeqCst),
        3,
        "recovery retried the interrupted embedding and reused the checkpoint from the paused publish"
    );
    assert_eq!(
        (
            status.complete,
            status.charged_tokens,
            status.provider_input_tokens,
            status.provider_output_tokens
        ),
        (1, 400, 400, 22)
    );
    assert_eq!(
        provider.script.calls.lock().await.len(),
        2,
        "resume reused completed chat"
    );
    Ok(())
}

#[tokio::test]
async fn top_down_conversion_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?.with_summary_index_key([7; 32]);
    let (historical, old) = legacy(&store).await?;
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute(
            "DELETE FROM rh_summary_state WHERE conversation_id = ?1",
            [historical.to_string()],
        )?;
        conn.execute(
            "UPDATE ros_summaries SET embedding = zeroblob(4096) WHERE id = ?1",
            [old.to_string()],
        )?;
        conn.execute(
            "INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, extracted_at)
                      VALUES (?1, 'done', 4, '2026-01-01T00:00:00Z')",
            [old.to_string()],
        )?;
    }
    let before = std::fs::read(&path)?;
    let report = memory_sqlite::SqliteStore::rehearse_conversion(&path, &[historical])?;
    assert_eq!(report.sampled_conversations, 1);
    assert_eq!(
        report
            .tables
            .get("ros_wiki_extractions")
            .ok_or("missing ros_wiki_extractions disposition")?
            .remapped,
        1
    );
    assert_eq!(
        report
            .tables
            .get("ros_summary_sources")
            .ok_or("missing ros_summary_sources disposition")?
            .deleted,
        1
    );
    assert_eq!(std::fs::read(&path)?, before);
    conversion(&store, historical, old, async {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute(
            "UPDATE rh_summary_conversion_items SET reservation_expires_at = '2020-01-01T00:00:00.000Z'
             WHERE conversation_id = ?1", [historical.to_string()],
        )?;
        Ok(())
    }).await?;
    let conn = rusqlite::Connection::open(&path)?;
    let receipts: Option<i32> = conn.query_row(
        "SELECT completed_version FROM ros_summary_deliveries
       WHERE conversation_id = ?1 AND consumer = 'wiki'",
        [historical.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, Some(4));
    let queued: i64 = conn.query_row(
        "SELECT count(*) FROM ros_jobs
        WHERE task = 'extract-wiki' AND json_extract(payload, '$.conversationId') = ?1",
        [historical.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(queued, 0);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_conversion_postgres(pool: sqlx::PgPool) -> TestResult {
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let store = memory_postgres::PostgresMemory::new(scoped).with_summary_index_key([7; 32]);
    Scope::Maintenance.run(async {
        let (historical, old) = legacy(&store).await?;
        sqlx::query("DELETE FROM rh_summary_state WHERE conversation_id = $1").bind(historical).execute(&pool).await?;
        sqlx::query("UPDATE rh_summaries SET embedding = array_fill(0.25::real, ARRAY[1024])::halfvec WHERE id = $1")
            .bind(old).execute(&pool).await?;
        sqlx::query("INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, owner_user_id)
                     SELECT id, 'done', 4, owner_user_id FROM rh_summaries WHERE id = $1")
            .bind(old).execute(&pool).await?;
        sqlx::query("INSERT INTO rh_entity_extractions (summary_id, status, pipeline_version, owner_user_id, extracted_at)
                     SELECT id, 'done', 5, owner_user_id, now() FROM rh_summaries WHERE id = $1")
            .bind(old).execute(&pool).await?;
        let report = store.rehearse_conversion(&[historical]).await?;
        assert_eq!(report.sampled_conversations, 1);
        assert_eq!(report.tables.get("rh_wiki_extractions").ok_or("missing rh_wiki_extractions disposition")?.remapped, 1);
        assert_eq!(report.tables.get("rh_entity_extractions").ok_or("missing rh_entity_extractions disposition")?.remapped, 1);
        assert_eq!(report.tables.get("rh_summary_sources").ok_or("missing rh_summary_sources disposition")?.deleted, 1);
        assert!(!store.summary_conversion().ok_or("conversion")?.status().await?.enabled);
        conversion(&store, historical, old, async {
            sqlx::query("UPDATE rh_summary_conversion_items SET reservation_expires_at = now() - interval '1 second'
                         WHERE conversation_id = $1").bind(historical).execute(&pool).await?;
            Ok(())
        }).await?;
        let receipts: Vec<(String, i32)> = sqlx::query_as(
            "SELECT consumer, completed_version FROM rh_summary_deliveries
             WHERE conversation_id = $1 ORDER BY consumer"
        ).bind(historical).fetch_all(&pool).await?;
        assert_eq!(receipts, [("entity".into(), 5), ("wiki".into(), 4)]);
        let hits = store.search(SearchQuery {
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
        }).await?;
        assert!(hits.results.iter().any(|hit| hit.passage.is_some()));
        Ok(())
    }).await
}

async fn exercise_inventory(
    first: &dyn SummaryConversion,
    second: &(impl SummaryConversion + SummaryStore + MemoryStore),
    preferred: Uuid,
    renewal: impl AsyncFnOnce(ConversionLease) -> TestResult,
) -> TestResult {
    let giant = Uuid::from_u128(201);
    first
        .control(ConversionControl::Start {
            rate: Some(1_000_000),
            include_giants: false,
        })
        .await?;
    assert!(first.claim(QUIET).await?.is_none());
    let progress = first.status().await?;
    assert_eq!(progress.discovered, 200);
    assert!(!progress.inventory_complete);
    assert!(progress.inventory_after.is_some());

    assert!(second.claim(QUIET).await?.is_none());
    let progress = second.status().await?;
    assert_eq!((progress.discovered, progress.giant_deferred), (201, 1));
    assert!(progress.inventory_complete);
    let reactivated = Uuid::from_u128(43);
    assert_eq!(
        capture(
            second,
            &reactivated.to_string(),
            short("This historical thread has fresh input.")
        )
        .await?,
        reactivated
    );
    assert!(
        !second
            .summary_state(reactivated)
            .await?
            .ok_or("new live revision")?
            .historical_only
    );

    let (one, two) = tokio::join!(first.claim(QUIET), second.claim(QUIET));
    let (one, two) = (one?, two?);
    assert_ne!(one.is_some(), two.is_some());
    let top = one.or(two).ok_or("ranked normal candidate")?;
    assert_eq!(
        top.conversation, preferred,
        "recent use outranks input age and giant recency"
    );
    renewal(top).await?;
    second.finish(top, ConversionOutcome::Complete).await?;
    assert_eq!(
        second.status().await?.discovered,
        200,
        "new input left the historical lane"
    );

    for _ in 0..198 {
        let normal = second
            .claim(QUIET)
            .await?
            .ok_or("another normal candidate")?;
        assert_ne!(normal.conversation, giant);
        second.finish(normal, ConversionOutcome::Complete).await?;
    }
    exercise_giant_admission(second, giant).await
}

async fn exercise_giant_admission(second: &dyn SummaryConversion, giant: Uuid) -> TestResult {
    assert!(
        second.claim(QUIET).await?.is_none(),
        "giants require opt-in"
    );
    second
        .control(ConversionControl::Resume {
            include_giants: true,
        })
        .await?;
    let lease = second.claim(QUIET).await?.ok_or("opted-in giant")?;
    assert_eq!(lease.conversation, giant);
    assert!(
        second
            .admit(
                lease,
                Some(ConversionCharge {
                    estimate: 950_000,
                    capacity: 1_000_000
                })
            )
            .await?
    );
    second
        .report(ConversionUsage {
            estimate: 950_000,
            input: 985_000,
            output: 200,
        })
        .await?;
    assert!(
        !second
            .admit(
                lease,
                Some(ConversionCharge {
                    estimate: 990_000,
                    capacity: 1_000_000
                })
            )
            .await?
    );
    let status = second.status().await?;
    assert_eq!(
        (
            status.charged_tokens,
            status.provider_input_tokens,
            status.provider_output_tokens
        ),
        (985_000, 985_000, 200)
    );
    second
        .finish(lease, ConversionOutcome::Fail("fixture error".into()))
        .await?;
    assert_eq!(
        second.status().await?.recent_failures,
        [(giant, "fixture error".into())]
    );
    second
        .control(ConversionControl::Retry {
            conversation: giant,
        })
        .await?;
    assert_eq!(second.status().await?.failed, 0);
    assert!(
        second.claim(QUIET).await?.is_none(),
        "bucket delay survived reservation release"
    );
    Ok(())
}

#[tokio::test]
async fn top_down_conversion_inventory_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let recent = Timestamp::now().rfc3339_offset();
    {
        let mut conn = rusqlite::Connection::open(&path)?;
        let tx = conn.transaction()?;
        for i in 1..=201 {
            let id = Uuid::from_u128(i).to_string();
            let message = Uuid::from_u128(1000 + i).to_string();
            let at = if i == 201 {
                "2020-04-01T00:00:00Z"
            } else {
                "2020-01-01T00:00:00Z"
            };
            tx.execute(
                "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at, owner_user_id)
                 VALUES (?1, ?1, 'fixture', ?2, ?2, 'owner')",
                (&id, at),
            )?;
            tx.execute(
                "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at, last_accessed_at, owner_user_id)
                 VALUES (?1, ?2, 'fixture', 'test', 'user', ?3, ?4, ?5, 'owner')",
                (&message, &id, if i == 201 { "x".repeat(3_000_001) } else { "invented entry".into() },
                 at, (i == 42).then_some(recent.as_str())),
            )?;
        }
        tx.commit()?;
    }
    let first = memory_sqlite::SqliteStore::open(&path)?;
    let second = memory_sqlite::SqliteStore::open(&path)?;
    exercise_inventory(&first, &second, Uuid::from_u128(42), async |lease| {
        let conn = rusqlite::Connection::open(&path)?;
        let read = || -> rusqlite::Result<(String, String)> {
            conn.query_row(
                "SELECT next_at, reservation_expires_at FROM rh_summary_conversion_items WHERE conversation_id = ?1",
                [lease.conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        };
        let (claim, expiry) = read()?;
        assert_eq!(claim, lease.claimed_at.rfc3339_nanos());
        assert_eq!(expiry, (lease.claimed_at + LEASE).rfc3339_millis());
        conn.execute(
            "UPDATE rh_summary_conversion_items SET reservation_expires_at = ?2 WHERE conversation_id = ?1",
            (lease.conversation.to_string(), (lease.claimed_at + jiff::SignedDuration::from_mins(5)).rfc3339_millis()),
        )?;
        assert!(second.admit(lease, None).await?);
        let (renewed_claim, renewed_expiry) = read()?;
        assert_eq!(renewed_claim, claim);
        assert!(renewed_expiry >= expiry);
        assert!(renewed_expiry <= (Timestamp::now() + LEASE).rfc3339_millis());
        Ok(())
    }).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_conversion_inventory_postgres(pool: sqlx::PgPool) -> TestResult {
    let conn = || {
        memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
            .connect_with(pool.connect_options().as_ref().clone())
    };
    let first = memory_postgres::PostgresMemory::new(conn().await?);
    let second = memory_postgres::PostgresMemory::new(conn().await?);
    let new = wire::time::parse_rfc3339("2020-04-01T00:00:00Z")?;
    let old = wire::time::parse_rfc3339("2020-01-01T00:00:00Z")?;
    let owner = testkit::owner()?;
    let ids: Vec<_> = (1..=201).map(Uuid::from_u128).collect();
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, created_at, updated_at, owner_user_id)
         SELECT id, id::text, 'fixture', CASE WHEN id = $2 THEN $3 ELSE $4 END, $4, $5
         FROM unnest($1::uuid[]) id",
    ).bind(&ids).bind(Uuid::from_u128(201)).bind(memory_postgres::time::PgTimestamp::from(new)).bind(memory_postgres::time::PgTimestamp::from(old)).bind(owner.as_str()).execute(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, created_at, owner_user_id)
         SELECT id, 'fixture', 'test', 'user', CASE WHEN id = $1 THEN repeat('x', 3000001) ELSE 'invented entry' END,
                created_at, owner_user_id FROM rh_conversations",
    ).bind(Uuid::from_u128(201)).execute(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_message_access (message_id, access_count, last_accessed_at)
         SELECT id, 1, now() FROM rh_messages WHERE conversation_id = $1",
    )
    .bind(Uuid::from_u128(42))
    .execute(&pool)
    .await?;
    Scope::Maintenance
        .run(exercise_inventory(
            &first,
            &second,
            Uuid::from_u128(42),
            async |lease| {
                use memory_postgres::time::PgTimestamp;
                let read = || {
                    sqlx::query_as::<_, (PgTimestamp, PgTimestamp)>(
                        "SELECT next_at, reservation_expires_at FROM rh_summary_conversion_items WHERE conversation_id = $1",
                    )
                    .bind(lease.conversation)
                    .fetch_one(&pool)
                };
                let (claim, expiry) = read().await?;
                assert_eq!(claim.to_jiff(), lease.claimed_at);
                assert_eq!(expiry.to_jiff(), lease.claimed_at + LEASE);
                sqlx::query(
                    "UPDATE rh_summary_conversion_items SET reservation_expires_at = next_at + interval '5 minutes' WHERE conversation_id = $1",
                )
                .bind(lease.conversation)
                .execute(&pool)
                .await?;
                assert!(second.admit(lease, None).await?);
                let (renewed_claim, renewed_expiry) = read().await?;
                assert_eq!(renewed_claim, claim);
                assert!(renewed_expiry.to_jiff() >= expiry.to_jiff());
                assert!(renewed_expiry.to_jiff() <= Timestamp::now() + LEASE);
                Ok(())
            },
        ))
        .await
}

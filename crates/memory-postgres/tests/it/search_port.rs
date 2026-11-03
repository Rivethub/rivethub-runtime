use std::sync::Arc;

use kernel::memory::MemoryEntry;
use kernel::memory::{BrowseWindow, MemoryHitSource, MemoryScope};
use kernel::memory::{Limit, MemoryStore, SearchMode, SearchQuery};
use kernel::model::Embedding;
use memory_postgres::{PostgresMemory, QueryEmbed, SearchSynthesis};
use pgvector::HalfVector;
use sqlx::PgPool;
use testkit::{FakeProvider, Script};
use tokio_util::sync::CancellationToken;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const TEXT: &str = "release checklist deployment safety health checks and rollback procedures";

fn query(mode: SearchMode) -> SearchQuery {
    SearchQuery {
        text: TEXT.into(),
        mode,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(1), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: Some(false),
        synthesize: Some(false),
    }
}

async fn seed(pool: &PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let entries = [
        "2026-10-11T00:00:00Z",
        "2026-10-12T00:00:00Z",
        "2026-10-13T00:00:00Z",
    ]
    .into_iter()
    .map(|time| {
        Ok(MemoryEntry {
            id: None,
            session_id: "search-port".into(),
            agent: "acme".into(),
            channel: "test".into(),
            role: Role::User,
            content: TEXT.into(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: Some(time.parse()?),
        })
    })
    .collect::<Result<Vec<_>, chrono::ParseError>>()?;
    memory.append(&testkit::owner()?, entries).await?;
    let mut tx = testkit::capture_tx(pool).await?;
    sqlx::query("INSERT INTO rh_summaries (conversation_id, content, kind, depth, message_count, created_at, owner_user_id) SELECT conversation_id, content, 'leaf', 0, 1, created_at, owner_user_id FROM rh_messages")
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO rh_summary_sources (summary_id, message_id) SELECT s.id, m.id FROM rh_summaries s JOIN rh_messages m ON m.conversation_id = s.conversation_id AND m.created_at = s.created_at")
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

fn bounds(query: &mut SearchQuery) -> TestResult {
    query.since = Some("2026-10-12T00:00:00Z".parse()?);
    query.before = Some("2026-10-13T00:00:00Z".parse()?);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn every_arm_filters_both_tables_before_limiting(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let vector = vec![0.5; 1024];
    for sql in [
        "UPDATE rh_messages SET embedding = $1",
        "UPDATE rh_summaries SET embedding = $1",
    ] {
        sqlx::query(sql)
            .bind(HalfVector::from_f32_slice(&vector))
            .execute(&pool)
            .await?;
    }
    let memory = PostgresMemory::new(pool.clone()).with_query_embed(QueryEmbed::new(
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector,
        }])])),
        None,
        None,
        None,
    ));
    for mode in [
        SearchMode::Hybrid,
        SearchMode::Fts,
        SearchMode::Regex,
        SearchMode::Trigram,
        SearchMode::Vector,
    ] {
        for scope in [MemoryScope::Messages, MemoryScope::Summaries] {
            let mut query = query(mode);
            query.scope = scope;
            bounds(&mut query)?;
            query.window = Some(BrowseWindow::Today);
            let outcome = memory
                .search(
                    query
                        .clone()
                        .resolve_window(chrono::Utc::now(), &chrono::Utc),
                )
                .await?;
            assert_eq!(outcome.results.len(), 1, "{mode:?} {scope:?}");
            assert_eq!(
                outcome.results.first().ok_or("missing result")?.created_at,
                query.since.ok_or("since")?
            );
            assert_eq!(
                outcome.results.first().ok_or("missing result")?.source,
                if scope == MemoryScope::Messages {
                    MemoryHitSource::Message
                } else {
                    MemoryHitSource::Summary
                }
            );
            assert_eq!(outcome.degraded, None);
            assert_eq!(outcome.expanded, Vec::new());
            query.since = query.before;
            assert!(matches!(
                memory.search(query).await,
                Err(kernel::memory::MemoryStoreError::InvalidRequest { .. })
            ));
        }
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_hit_names_its_session(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let title = |outcome: &kernel::memory::SearchOutcome| {
        outcome.results.first().and_then(|hit| hit.title.clone())
    };
    let untitled = memory.search(query(SearchMode::Fts)).await?;
    assert_eq!(title(&untitled), kernel::memory::summary_title(TEXT));
    sqlx::query("UPDATE rh_conversations SET title = 'Release safety review'")
        .execute(&pool)
        .await?;
    let titled = memory.search(query(SearchMode::Fts)).await?;
    assert_eq!(title(&titled).as_deref(), Some("Release safety review"));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn expansion_is_opt_out_and_retains_time_bounds(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    sqlx::query("INSERT INTO rh_summary_sources (summary_id, message_id) SELECT s.id, m.id FROM rh_summaries s CROSS JOIN rh_messages m ON CONFLICT DO NOTHING")
        .execute(&pool).await?;
    let memory = PostgresMemory::new(pool);
    for expand in [None, Some(true), Some(false)] {
        let mut query = query(SearchMode::Fts);
        query.scope = MemoryScope::Summaries;
        query.expand = expand;
        bounds(&mut query)?;
        let outcome = memory.search(query).await?;
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.expanded.len(), usize::from(expand != Some(false)));
        for entry in outcome.expanded {
            assert_eq!(entry.created_at, Some("2026-10-12T00:00:00Z".parse()?));
            assert_eq!(entry.content, TEXT);
        }
        assert_eq!(outcome.degraded, None);
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn runtime_embedding_failure_reaches_the_port_with_results(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    for mode in [SearchMode::Hybrid, SearchMode::Vector, SearchMode::Fts] {
        let memory = PostgresMemory::new(pool.clone()).with_query_embed(QueryEmbed::new(
            Arc::new(FakeProvider::new(vec![Script::EmbedHttpStatus(503)])),
            None,
            None,
            None,
        ));
        let outcome = memory.search(query(mode)).await?;
        assert_eq!(outcome.results.len(), 1);
        let notice = outcome.degraded.ok_or("missing notice")?;
        assert_eq!(notice.reason, "provider request failed: http 503");
        assert_eq!(
            notice.effect,
            "Keyword / FTS ranking only — not meaning-based."
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn synthesis_uses_the_injected_provider_and_honours_cancellation(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let provider = Arc::new(FakeProvider::new(vec![Script::Chat(
        kernel::model::LlmResponse {
            r#type: kernel::model::LlmResponseKind::Text,
            content: Some("Deployment is ready.".into()),
            tool_calls: None,
            usage: None,
            finish_reason: None,
        },
    )]));
    let cancel = CancellationToken::new();
    let memory = PostgresMemory::new(pool.clone()).with_search_synthesis(SearchSynthesis::new(
        provider.clone(),
        "summarizer".into(),
        cancel.clone(),
    ));
    let mut query = query(SearchMode::Fts);
    query.synthesize = Some(true);
    assert_eq!(
        memory
            .search(
                query
                    .clone()
                    .resolve_window(chrono::Utc::now(), &chrono::Utc)
            )
            .await?
            .synthesis,
        Some("Deployment is ready.".into())
    );
    let requests = provider.chat_requests().await;
    let [request] = requests.as_slice() else {
        return Err("one synthesis request".into());
    };
    assert_eq!(request.model, "summarizer");
    let [system, user] = request.messages.as_slice() else {
        return Err("system and user messages".into());
    };
    assert_eq!(system.role, Role::System);
    let wire::message::MessageContent::Text(evidence) = &user.content else {
        panic!("text evidence")
    };
    assert!(evidence.contains(TEXT));
    assert!(evidence.contains("Memory evidence:"));
    cancel.cancel();
    assert!(memory.search(query.clone()).await.is_err());
    assert_eq!(provider.chat_requests().await.len(), 1);
    query.synthesize = Some(false);
    assert_eq!(
        memory
            .search(
                query
                    .clone()
                    .resolve_window(chrono::Utc::now(), &chrono::Utc)
            )
            .await?
            .synthesis,
        None
    );
    query.synthesize = Some(true);
    let unwired = PostgresMemory::new(pool);
    assert!(matches!(
        unwired.search(query).await,
        Err(kernel::memory::MemoryStoreError::InvalidRequest { .. })
    ));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn window_filters_and_each_explicit_bound_overrides_it(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    sqlx::query(
        "UPDATE rh_messages SET created_at = now() WHERE created_at = '2026-10-12T00:00:00Z'",
    )
    .execute(&pool)
    .await?;
    let mut query = query(SearchMode::Regex);
    query.scope = MemoryScope::Messages;
    query.window = Some(BrowseWindow::Last24h);
    assert_eq!(
        memory
            .search(
                query
                    .clone()
                    .resolve_window(chrono::Utc::now(), &chrono::Utc)
            )
            .await?
            .results
            .len(),
        1
    );
    query.before = Some("2026-10-12T00:00:00Z".parse()?);
    let outcome = memory
        .search(
            query
                .clone()
                .resolve_window(chrono::Utc::now(), &chrono::Utc),
        )
        .await?;
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(
        outcome.results.first().ok_or("missing result")?.created_at,
        "2026-10-11T00:00:00Z".parse::<chrono::DateTime<chrono::Utc>>()?
    );
    query.before = None;
    query.since = Some("2100-01-01T00:00:00Z".parse()?);
    assert_eq!(memory.search(query).await?.results, Vec::new());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn chunk_only_parents_respect_time_bounds(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let vector = vec![0.5; 1024];
    sqlx::query("INSERT INTO rh_message_chunks (message_id, idx, char_start, char_end, content, embedding, owner_user_id) SELECT id, 0, 0, 7, 'release', $1, owner_user_id FROM rh_messages")
        .bind(HalfVector::from_f32_slice(&vector)).execute(&pool).await?;
    let memory = PostgresMemory::new(pool).with_query_embed(QueryEmbed::new(
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector,
        }])])),
        None,
        None,
        None,
    ));
    let mut query = query(SearchMode::Vector);
    query.scope = MemoryScope::Messages;
    bounds(&mut query)?;
    let outcome = memory.search(query).await?;
    let [hit] = outcome.results.as_slice() else {
        return Err("one chunk parent".into());
    };
    assert_eq!(
        hit.created_at,
        "2026-10-12T00:00:00Z".parse::<chrono::DateTime<chrono::Utc>>()?
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn synthesis_without_text_is_an_error_not_a_silent_default(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let provider = Arc::new(FakeProvider::new(vec![Script::Chat(
        kernel::model::LlmResponse {
            r#type: kernel::model::LlmResponseKind::Text,
            content: None,
            tool_calls: None,
            usage: None,
            finish_reason: None,
        },
    )]));
    let memory = PostgresMemory::new(pool).with_search_synthesis(SearchSynthesis::new(
        provider,
        "summarizer".into(),
        CancellationToken::new(),
    ));
    let mut query = query(SearchMode::Fts);
    query.synthesize = Some(true);
    let error = memory
        .search(query)
        .await
        .err()
        .ok_or("synthesis must fail")?;
    assert_eq!(
        error.to_string(),
        "memory query failed: memory synthesis returned no text"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn close_waits_for_access_writes_and_wakes_every_waiter(pool: PgPool) -> TestResult {
    use std::future::{Future as _, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    seed(&pool).await?;
    let store: Arc<dyn MemoryStore> = Arc::new(PostgresMemory::new(pool.clone()));
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE rh_message_access, rh_summary_access IN SHARE MODE")
        .execute(&mut *blocker)
        .await?;
    let mut search = query(SearchMode::Fts);
    search.text = "release".into();
    search.limit = Limit::clamped(Some(10), 10, 1..=50);
    let outcome = store.search(search).await?;
    assert_eq!(outcome.results.len(), 6);
    let closing = store.close();
    tokio::pin!(closing);
    poll_fn(|cx| {
        assert!(closing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    blocker.commit().await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let (first, second, third) = tokio::join!(closing, store.close(), store.close());
        first?;
        second?;
        third
    })
    .await??;
    let counts: Vec<i32> = sqlx::query_scalar(
        "SELECT access_count FROM rh_message_access UNION ALL SELECT access_count FROM rh_summary_access",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(counts, vec![1; 6]);
    store.close().await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn close_reports_background_write_failure(pool: PgPool) -> TestResult {
    use std::time::Duration;

    seed(&pool).await?;
    sqlx::query(
        "ALTER TABLE rh_message_access ADD CONSTRAINT refuse_access CHECK (access_count = 0)",
    )
    .execute(&pool)
    .await?;
    let store = PostgresMemory::new(pool);
    let mut search = query(SearchMode::Fts);
    search.scope = MemoryScope::Messages;
    search.text = "release".into();
    assert_eq!(store.search(search).await?.results.len(), 1);
    let result = tokio::time::timeout(Duration::from_secs(10), store.close()).await?;
    assert!(matches!(
        result,
        Err(kernel::memory::MemoryStoreError::Unavailable(_))
    ));
    assert!(store.close().await.is_err());
    Ok(())
}

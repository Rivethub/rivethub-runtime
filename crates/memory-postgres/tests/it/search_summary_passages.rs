use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use kernel::memory::{Limit, MemoryScope, MemoryStore as _, SearchMode, SearchQuery, SummaryKind};
use kernel::tenancy::Scope;
use memory_postgres::{PostgresMemory, QueryEmbed};
use sqlx::PgPool;
use testkit::{FakeProvider, Script};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FIRST: &str =
    "## Orchard valves\nThe marmalade pressure valve fits the /srv/amber-cask bypass. ";
const SECOND: &str = "The fallback marmalade heater belongs to the later outdoor work. ";
const OWNER: &str = "00000000-0000-4000-8000-000000000020";
const FIRST_PASSAGE: &str = "00000000-0000-4000-8000-000000000030";
const SECOND_PASSAGE: &str = "00000000-0000-4000-8000-000000000031";
const LEGACY: &str = "00000000-0000-4000-8000-000000000040";

const SEED: &str = r"
    INSERT INTO rh_conversations (id, session_key, agent, channel, title, owner_user_id) VALUES
        ('00000000-0000-4000-8000-000000000001', 'orchard', 'fixture', 'cli', 'Orchard plumbing', 'alice'),
        ('00000000-0000-4000-8000-000000000002', 'legacy', 'fixture', 'cli', NULL, 'alice'),
        ('00000000-0000-4000-8000-000000000003', 'repair', 'fixture', 'cli', NULL, 'alice');
    INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
    SELECT md5('source'||i)::uuid, '00000000-0000-4000-8000-000000000001', 'fixture', 'cli',
        'user', repeat('Hand-seeded orchard source '||i||' with enough words. ', 20),
        ('2024-01-01'::timestamptz + (i-1)*interval '1 year'), 'alice'
    FROM generate_series(1,3) i;
    INSERT INTO rh_summaries (id, conversation_id, depth, content, kind, message_count, earliest_at,
        latest_at, created_at, owner_user_id, embedding)
    VALUES ('00000000-0000-4000-8000-000000000040','00000000-0000-4000-8000-000000000002',
        0, 'The previous marmalade design used a brass elbow on the side of the pipe.',
        'leaf', 1, '2020-01-01', '2020-01-02', '2099-01-01', 'alice',
        array_fill(1::real,ARRAY[1024])::halfvec);
    INSERT INTO rh_summaries (id, conversation_id, depth, content, kind, message_count, earliest_at,
        latest_at, created_at, owner_user_id, pipeline_version, summary_ordinal, source_first,
        source_last, input_key,index_needs_repair)
    SELECT ('00000000-0000-4000-8000-00000000002'||i)::uuid,
        ('00000000-0000-4000-8000-00000000000'||(i*2+1))::uuid,
        2, '', 'session', 3, '2024-01-01', '2026-01-01','2099-01-01','alice',7,
        0,1,3,decode(repeat('ab',32),'hex'),i=1
    FROM generate_series(0,1) i;
    INSERT INTO rh_summary_sources (summary_id, message_id, ordinal)
    SELECT '00000000-0000-4000-8000-000000000020', md5('source'||i)::uuid, i
    FROM generate_series(1,3) i;
    INSERT INTO rh_summaries (id, conversation_id, parent_id, depth, content, kind, message_count,
        earliest_at,latest_at,owner_user_id,pipeline_version,summary_ordinal,source_first,source_last,
        input_key,range_verified,section_char_start,section_char_end)
    SELECT md5('section'||i)::uuid, s.conversation_id,s.id,0,
        CASE i WHEN 1 THEN E'## Orchard valves\nsectionunique' ELSE E'## Pressure setup\nsectionunique' END,
        'section',1, '2024-01-01'::timestamptz+(i-1)*interval '1 year',
        '2024-01-01'::timestamptz+(i-1)*interval '1 year',
        'alice',7,i,i,i,s.input_key,i=1,(i-1)*5,i*5
    FROM rh_summaries s CROSS JOIN generate_series(1,2) i
    WHERE s.id='00000000-0000-4000-8000-000000000020';
";

async fn seed(pool: &PgPool) -> TestResult {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(SEED).execute(&mut *tx).await?;
    sqlx::query("UPDATE rh_summaries SET content=$1 WHERE id IN ($2,$3)")
        .bind(format!("{FIRST}{SECOND}"))
        .bind(Uuid::parse_str(OWNER)?)
        .bind(Uuid::parse_str("00000000-0000-4000-8000-000000000021")?)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO rh_summary_passages
          (id,record_id,ordinal,char_start,char_end,content,earliest_at,latest_at,index_key,embedding)
         SELECT v.id::uuid,s.id,v.ordinal,v.start_at,v.start_at+length(v.content),v.content,
            s.earliest_at,v.latest::timestamptz,s.input_key,array_fill(1::real,ARRAY[1024])::halfvec
         FROM (VALUES
             ($1::text, $2::text, 0, 0, $3::text, '2025-01-01'),
             ($4, $2, 1, length($3), $5, '2026-01-01'),
             ('00000000-0000-4000-8000-000000000032', '00000000-0000-4000-8000-000000000021',
              0, 0, $3, '2025-01-01')
         ) v(id,owner,ordinal,start_at,content,latest)
         JOIN rh_summaries s ON s.id=v.owner::uuid",
    )
    .bind(FIRST_PASSAGE)
    .bind(OWNER)
    .bind(FIRST)
    .bind(SECOND_PASSAGE)
    .bind(SECOND)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

fn query(text: &str, mode: SearchMode) -> SearchQuery {
    SearchQuery {
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
        text: text.to_owned(),
        mode,
        scope: MemoryScope::Summaries,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(40), 20, 1..=50),
    }
}

fn at(year: u16) -> Result<DateTime<Utc>, chrono::ParseError> {
    format!("{year}-01-01T00:00:00Z").parse()
}

fn wired(pool: PgPool, count: usize) -> PostgresMemory {
    let provider = FakeProvider::new(
        (0..count)
            .map(|_| {
                Script::Embed(vec![kernel::model::Embedding {
                    vector: vec![1.0; 1024],
                }])
            })
            .collect(),
    );
    PostgresMemory::new(pool).with_query_embed(QueryEmbed::new(
        Arc::new(provider),
        None,
        None,
        None,
    ))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_fuses_passages_and_legacy_with_provenance(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = wired(pool.clone(), 2);
    let outcome = memory
        .search(query("marmalade", SearchMode::Hybrid))
        .await?;
    assert!(outcome.degraded.is_none());
    assert_eq!(outcome.results.len(), 3);
    assert!(
        outcome
            .results
            .iter()
            .any(|hit| hit.id == LEGACY && hit.passage.is_none())
    );
    let hit = outcome
        .results
        .iter()
        .find(|hit| hit.detail_id() == FIRST_PASSAGE)
        .expect("first passage");
    assert_eq!(hit.id, OWNER);
    assert_eq!(hit.passage.as_ref().expect("passage").ordinal, 0);
    assert_eq!(hit.content, FIRST);
    assert_eq!(hit.title.as_deref(), Some("Orchard plumbing"));
    assert_eq!(hit.created_at, at(2025)?);

    let detail = outcome.summaries.get(hit.detail_id()).expect("detail");
    assert_eq!(detail.kind, SummaryKind::Session);
    assert_eq!(detail.latest_at, Some(at(2025)?));
    assert_eq!(detail.children, []);
    assert_eq!(
        detail
            .sections
            .iter()
            .map(|section| (
                section.heading.as_str(),
                section.source_first,
                section.range_verified
            ))
            .collect::<Vec<_>>(),
        [("Orchard valves", 1, true), ("Pressure setup", 2, false)]
    );
    assert_eq!(detail.sources.len(), 2);
    assert!(
        detail
            .sources
            .iter()
            .all(|source| source.content.len() == 500)
    );
    assert_eq!(
        detail.sources.last().expect("second source").created_at,
        Some(at(2025)?)
    );
    let fallback = outcome
        .summaries
        .get(SECOND_PASSAGE)
        .expect("fallback detail");
    assert_eq!(fallback.sections, []);
    assert_eq!(fallback.sources.len(), 3);
    assert_eq!(outcome.expanded, []);

    memory.close().await?;
    let counts: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT summary_id,access_count FROM rh_summary_access ORDER BY summary_id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        counts,
        [(Uuid::parse_str(OWNER)?, 1), (Uuid::parse_str(LEGACY)?, 1)]
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_uses_source_time_and_pins_legacy_scoring(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = wired(pool, 2);
    let mut q = query("marmalade", SearchMode::Hybrid);
    q.since = Some(at(2098)?);
    let outcome = memory.search(q).await?;
    let [legacy] = outcome.results.as_slice() else {
        return Err("expected only the legacy hit".into());
    };
    assert_eq!(legacy.id, LEGACY);
    assert_eq!(legacy.created_at, at(2099)?);
    let expected_legacy = (2.0 / 21.0) * 1.36 * 1.3;
    assert!((legacy.relevance_score - expected_legacy).abs() < 1e-12);

    let mut q = query("marmalade", SearchMode::Vector);
    q.since = Some(at(2025)?);
    q.before = Some(at(2026)?);
    let outcome = memory.search(q).await?;
    let [passage] = outcome.results.as_slice() else {
        return Err("expected only the older passage".into());
    };
    assert_eq!(passage.detail_id(), FIRST_PASSAGE);
    let age = (Utc::now() - at(2025)?).to_std()?.as_secs_f64() / 86400.0;
    let expected = kernel::memory_core::composite_score(
        1.0,
        1.0,
        kernel::memory_core::recency_boost(age, 0, 0.6),
    );
    assert!((passage.relevance_score - expected).abs() < 1e-6);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_literal_and_full_text_exclude_sections_and_incomplete_owners(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(pool);
    for mode in [SearchMode::Trigram, SearchMode::Regex] {
        let mut q = query("/srv/amber-cask", mode);
        q.expand = Some(false);
        let outcome = memory.search(q).await?;
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(
            outcome.results.first().expect("literal hit").detail_id(),
            FIRST_PASSAGE
        );
        let detail = outcome
            .summaries
            .get(FIRST_PASSAGE)
            .expect("literal detail");
        assert_eq!(detail.sections.len(), 2);
        assert_eq!(detail.sources, []);
    }
    let outcome = memory
        .search(query("sectionunique", SearchMode::Fts))
        .await?;
    assert_eq!(outcome.results, []);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_respects_the_limit_without_collapsing_duplicate_passages(
    pool: PgPool,
) -> TestResult {
    seed(&pool).await?;
    let session = Uuid::parse_str(OWNER)?;
    let text = "Clementine stash for the fall harvest at the community orchard. ";
    let owner: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_summaries (id,conversation_id,parent_id,depth,kind,content,message_count,
             earliest_at,latest_at,owner_user_id,pipeline_version,summary_ordinal,source_first,source_last,
             issued_part_ordinal,input_token_estimate,input_key)
         SELECT md5('part')::uuid, conversation_id,id,1,'part',repeat($1,45),message_count,
             earliest_at,latest_at,owner_user_id,7,1,source_first,source_last,1,1024,input_key
         FROM rh_summaries WHERE id=$2 RETURNING id",
    ).bind(text).bind(session).fetch_one(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_summary_passages
         (id,record_id,ordinal,char_start,char_end,content,earliest_at,latest_at,index_key,embedding)
         SELECT md5('bulk'||i)::uuid,s.id,i,i*length($1),(i+1)*length($1),$1,s.earliest_at,s.latest_at,
             s.input_key,array_fill(1::real,ARRAY[1024])::halfvec
         FROM rh_summaries s CROSS JOIN generate_series(0,44) i WHERE s.id=$2",
    ).bind(text).bind(owner).execute(&pool).await?;
    let memory = PostgresMemory::new(pool);
    let mut q = query("clementine", SearchMode::Fts);
    q.expand = Some(false);
    let outcome = memory.search(q).await?;
    let hits = outcome.results;
    let owner = owner.to_string();
    assert_eq!(hits.len(), 40);
    assert!(
        outcome
            .summaries
            .values()
            .all(|detail| detail.kind == SummaryKind::Part)
    );
    assert!(
        hits.iter()
            .all(|hit| hit.id == owner && hit.content == text)
    );
    assert_eq!(
        hits.iter()
            .map(kernel::memory::MemorySearchResult::detail_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        40
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_uses_one_snapshot_during_tree_replacement(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let mut writer = pool.begin().await?;
    sqlx::query("LOCK TABLE rh_summary_sources IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await?;
    let memory = PostgresMemory::new(pool.clone());
    let search = tokio::spawn(async move {
        memory
            .search(query("orchard valves", SearchMode::Fts))
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                 WHERE datname=current_database() AND wait_event_type='Lock'
                   AND query LIKE 'SELECT p.id AS summary_id, picked.%')",
            )
            .fetch_one(&pool)
            .await?;
            if waiting {
                break Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    sqlx::query("DELETE FROM rh_summaries WHERE parent_id=$1 OR id=$1")
        .bind(Uuid::parse_str(OWNER)?)
        .execute(&mut *writer)
        .await?;
    sqlx::query(
        "INSERT INTO rh_summaries (id,conversation_id,depth,kind,content,message_count,earliest_at,latest_at,
            owner_user_id,pipeline_version,summary_ordinal,source_first,source_last,input_key,index_needs_repair)
         VALUES (md5('replacement')::uuid,'00000000-0000-4000-8000-000000000001',2,'session',
            'Replacement created while passage search is running',3,'2024-01-01','2026-01-01','alice',
            7,0,1,3,decode(repeat('ab',32),'hex'),true)",
    ).execute(&mut *writer).await?;
    writer.commit().await?;
    let result = search.await??;
    assert_eq!(
        result.results.first().expect("snapshot hit").detail_id(),
        FIRST_PASSAGE
    );
    let detail = result
        .summaries
        .get(FIRST_PASSAGE)
        .expect("snapshot detail");
    assert_eq!(detail.sections.len(), 2);
    assert_eq!(detail.sources.len(), 2);
    let current = PostgresMemory::new(pool)
        .search(query("orchard valves", SearchMode::Fts))
        .await?;
    assert_eq!(current.results, []);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_search_does_not_expose_hidden_passages(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = PostgresMemory::new(crate::common::probe(&pool).await?);
    for (name, expected) in [("alice", 1), ("bob", 0)] {
        let scope = Scope::Viewer(crate::visibility::viewer(name)?);
        let hits = scope
            .run(memory.search(query("/srv/amber-cask", SearchMode::Trigram)))
            .await?;
        assert_eq!(hits.results.len(), expected);
    }
    Ok(())
}

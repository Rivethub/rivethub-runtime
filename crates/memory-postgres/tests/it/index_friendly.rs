use crate::common;

use kernel::memory::MemoryScope;
use kernel::memory::jobs::MemoryJobStore as _;
use kernel::memory::{Limit, MemoryStore as _, SearchMode, SearchQuery};
use kernel::queue::{ClaimSelector, JobQueue as _, WorkerRole};
use kernel::wiki::WikiRedLink;
use memory_postgres::{PostgresMemory, PostgresQueue};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CONVERSATION: &str = "00000000-0000-4000-8000-0000000000aa";

const PREVIOUS_UNCHUNKED: &str = "SELECT m.id FROM rh_messages m
     CROSS JOIN LATERAL (SELECT
         length(btrim(coalesce(m.content, ''), E' \\t\\n\\r\\f\\x0B')) AS content_chars,
         length(btrim(coalesce(m.tool_result, ''), E' \\t\\n\\r\\f\\x0B')) AS tool_chars) l
     WHERE NOT EXISTS (SELECT 1 FROM rh_message_chunks c WHERE c.message_id = m.id)
       AND COALESCE(m.embed_status, '') <> 'unembeddable'
       AND (CASE WHEN l.content_chars = 0 THEN least(l.tool_chars, $2)
                 WHEN l.tool_chars = 0 THEN l.content_chars
                 ELSE l.content_chars + least(l.tool_chars, $2) + 1 END) > $1
     ORDER BY m.created_at DESC LIMIT $3";

const PREVIOUS_RED_LINKS: &str =
    "SELECT entity, array_agg(DISTINCT t.slug ORDER BY t.slug) AS referenced_by
     FROM rh_wiki_topics t CROSS JOIN LATERAL unnest(t.entities) entity
     WHERE NOT EXISTS (
         SELECT 1 FROM rh_wiki_topics known
         WHERE lower(known.slug) = lower(entity) OR lower(known.title) = lower(entity)
         OR EXISTS (SELECT 1 FROM unnest(known.aliases) alias WHERE lower(alias) = lower(entity))
     ) AND NOT EXISTS (
         SELECT 1 FROM rh_wiki_redirects r WHERE lower(r.from_slug) = lower(entity)
     )
     GROUP BY entity ORDER BY count(DISTINCT t.slug) DESC, entity LIMIT $1";

const PREVIOUS_CLAIM_ORDER: &str = "SELECT j.id FROM graphile_worker._private_jobs j
     JOIN graphile_worker._private_tasks t ON t.id = j.task_id
     WHERE t.identifier = ANY($1)
       AND j.attempts < j.max_attempts
       AND (j.locked_at IS NULL OR j.locked_at < now() - interval '4 hours')
       AND j.run_at <= now()
     ORDER BY j.priority, j.run_at, j.id";

async fn conversation(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_conversations (id, session_key, agent, channel, owner_user_id)
         VALUES ($1::uuid, 'index-friendly', 'acme', 'test', 'owner')",
    )
    .bind(CONVERSATION)
    .execute(pool)
    .await?;
    Ok(())
}

async fn message(
    pool: &PgPool,
    minute: u16,
    content: &str,
    tool_result: Option<&str>,
    status: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::from_u128(u128::from(minute));
    let mut tx = testkit::capture_tx(pool).await?;
    sqlx::query(
        "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, tool_result, embed_status, created_at, owner_user_id)
         VALUES ($1, $2::uuid, 'acme', 'test', 'user', $3, $4, $5,
                 '2026-10-01T00:00:00Z'::timestamptz + make_interval(mins => $6), 'owner')",
    )
    .bind(id)
    .bind(CONVERSATION)
    .bind(content)
    .bind(tool_result)
    .bind(status)
    .bind(i32::from(minute))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn summary(pool: &PgPool, minute: u16, content: &str) -> Result<Uuid, sqlx::Error> {
    let mut tx = testkit::capture_tx(pool).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO rh_summaries (conversation_id, content, kind, depth, message_count, created_at, owner_user_id)
         VALUES ($1::uuid, $2, 'leaf', 0, 1,
                 '2026-10-01T00:00:00Z'::timestamptz + make_interval(mins => $3), 'owner')
         RETURNING id",
    )
    .bind(CONVERSATION)
    .bind(content)
    .bind(i32::from(minute))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

fn search(text: &str, mode: SearchMode, scope: MemoryScope) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        mode,
        scope,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(20), 20, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: Some(false),
        synthesize: Some(false),
    }
}

async fn hit_ids(
    memory: &PostgresMemory,
    query: &SearchQuery,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(memory
        .search_outcome_for(query)
        .await?
        .hits
        .into_iter()
        .map(|hit| hit.id)
        .collect())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn literal_search_matches_tokens_in_content_tool_results_and_summaries(
    pool: PgPool,
) -> TestResult {
    conversation(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let drive = message(&pool, 1, "ordered a tvrt M.2 drive for the nas", None, None).await?;
    let listing = message(
        &pool,
        2,
        "listed the block devices",
        Some("nvme0n1 is the m.2 slot at /dev/nvme0n1"),
        None,
    )
    .await?;
    message(&pool, 3, "an m2 drive without the dot", None, None).await?;
    message(&pool, 4, "m.2", Some(""), Some("unembeddable")).await?;
    let percent = message(&pool, 5, "the import reached 100%_done today", None, None).await?;
    message(
        &pool,
        6,
        "the import reached 100 percent xdone today",
        None,
        None,
    )
    .await?;
    let path = message(&pool, 7, "edited C:\\work\\m.2\\notes.txt", None, None).await?;
    let arrived = summary(&pool, 8, "the tvrt m.2 drive arrived").await?;
    summary(&pool, 9, "nothing literal in this summary").await?;
    let latest = path.to_string();
    let short = Uuid::from_u128(4).to_string();

    assert_eq!(
        hit_ids(
            &memory,
            &search("tvrt m.2 drive", SearchMode::Trigram, MemoryScope::Both)
        )
        .await?,
        vec![
            arrived.to_string(),
            latest.clone(),
            short.clone(),
            listing.to_string(),
            drive.to_string(),
        ]
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search(
                "m.2 /dev/nvme0n1",
                SearchMode::Trigram,
                MemoryScope::Messages
            )
        )
        .await?,
        vec![
            listing.to_string(),
            latest.clone(),
            short,
            drive.to_string()
        ]
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search("100%_done", SearchMode::Trigram, MemoryScope::Messages)
        )
        .await?,
        vec![percent.to_string()]
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search("C:\\work\\m.2", SearchMode::Trigram, MemoryScope::Messages)
        )
        .await?,
        vec![latest]
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search(
                "  import reached  ",
                SearchMode::Trigram,
                MemoryScope::Messages
            )
        )
        .await?,
        vec![Uuid::from_u128(6).to_string(), percent.to_string()]
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search("   ", SearchMode::Trigram, MemoryScope::Both)
        )
        .await?,
        Vec::<String>::new()
    );
    assert!(
        hit_ids(
            &memory,
            &search("tvrt m.2 drive", SearchMode::Hybrid, MemoryScope::Both)
        )
        .await?
        .contains(&drive.to_string())
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_common_literal_answers_with_its_newest_matches(pool: PgPool) -> TestResult {
    conversation(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    for minute in 1..=70 {
        message(
            &pool,
            minute,
            &format!("host 203.0.113.70 at {minute:04}"),
            None,
            None,
        )
        .await?;
    }
    message(&pool, 71, "host 203.0.113.71 at 0071", None, None).await?;
    let output = message(
        &pool,
        72,
        "host 203.0.113.72 at 0072",
        Some("ping 203.0.113.70"),
        None,
    )
    .await?;
    let mut expected = vec![output.to_string()];
    expected.extend(
        (52..=70)
            .rev()
            .map(|minute| Uuid::from_u128(minute).to_string()),
    );
    assert_eq!(
        hit_ids(
            &memory,
            &search("203.0.113.70", SearchMode::Trigram, MemoryScope::Messages)
        )
        .await?,
        expected
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unchunked_returns_the_rows_of_the_full_length_scan(pool: PgPool) -> TestResult {
    conversation(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let wide = "\u{e9}".repeat(15);
    let wider = "\u{e9}".repeat(21);
    let fixtures: Vec<(String, Option<String>, Option<&str>)> = vec![
        ("a".repeat(21), None, None),
        ("a".repeat(20), None, None),
        (wide.clone(), None, None),
        (wider.clone(), None, None),
        (format!("   {wide}   \n\t"), None, None),
        (" ".repeat(64), None, None),
        (String::new(), Some(wide.clone()), None),
        (String::new(), Some(wider.clone()), None),
        ("b".repeat(10), Some("c".repeat(9)), None),
        ("b".repeat(10), Some("c".repeat(10)), None),
        (wide.clone(), Some(" ".repeat(40)), None),
        ("x".repeat(19), Some("\u{1f600}".repeat(3)), None),
        ("a".repeat(30), None, Some("unembeddable")),
        ("a".repeat(30), None, Some("failed")),
        (" \u{b}\u{c}\r\n".repeat(10), Some("d".repeat(30)), None),
    ];
    let mut chunked = None;
    for (minute, (content, tool_result, status)) in (1u16..).zip(&fixtures) {
        let id = message(&pool, minute, content, tool_result.as_deref(), *status).await?;
        chunked.get_or_insert(id);
    }
    sqlx::query(
        "INSERT INTO rh_message_chunks (message_id, idx, char_start, char_end, content, embed_status, owner_user_id)
         VALUES ($1, 0, 0, 21, 'x', 'embedded', 'owner')",
    )
    .bind(chunked.ok_or("no fixture rows")?)
    .execute(&pool)
    .await?;
    for (chars, cap, limit) in [
        (20u32, 32_768u32, 100u32),
        (20, 15, 100),
        (20, 4, 100),
        (5, 2, 100),
        (20, 32_768, 3),
        (0, 0, 100),
    ] {
        let previous: Vec<Uuid> = sqlx::query_scalar(PREVIOUS_UNCHUNKED)
            .bind(i64::from(chars))
            .bind(i64::from(cap))
            .bind(i64::from(limit))
            .fetch_all(&pool)
            .await?;
        let current: Vec<Uuid> = memory
            .unchunked(limit, chars, cap)
            .await?
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert!(!previous.is_empty(), "{chars}/{cap}/{limit}");
        assert_eq!(current, previous, "{chars}/{cap}/{limit}");
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_red_links_match_the_per_entity_scan(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let topics: [(&str, &str, &[&str], &[&str]); 5] = [
        (
            "tvrt",
            "TVRT Server",
            &["The Box"],
            &["NAS", "m.2 Drive", "Ghost", "tvrt server", "rivet"],
        ),
        (
            "nas",
            "Network Storage",
            &[],
            &["the box", "Ghost", "ZFS", "zfs", "Moved"],
        ),
        (
            "rivet",
            "Rivet",
            &["RivetOS", "Rivet Hub"],
            &["ghost", "rivethub", "RIVETOS", "zfs"],
        ),
        ("zpool", "ZPool", &["Pool"], &["ZFS", "Ghost", "Orphan"]),
        ("lonely", "Lonely", &[], &[]),
    ];
    for (slug, title, aliases, entities) in topics {
        sqlx::query(
            "INSERT INTO rh_wiki_topics (slug, title, aliases, entities) VALUES ($1, $2, $3, $4)",
        )
        .bind(slug)
        .bind(title)
        .bind(aliases)
        .bind(entities)
        .execute(&pool)
        .await?;
    }
    sqlx::query("INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('MOVED', 'nas')")
        .execute(&pool)
        .await?;
    for limit in [1u32, 2, 200] {
        let previous: Vec<(String, Vec<String>)> = sqlx::query_as(PREVIOUS_RED_LINKS)
            .bind(i64::from(limit))
            .fetch_all(&pool)
            .await?;
        let expected: Vec<WikiRedLink> = previous
            .into_iter()
            .map(|(entity, referenced_by)| WikiRedLink {
                entity,
                referenced_by,
            })
            .collect();
        assert_eq!(
            memory.wiki_gaps(limit).await?.red_links,
            expected,
            "{limit}"
        );
    }
    let mut all: Vec<(String, usize)> = memory
        .wiki_gaps(200)
        .await?
        .red_links
        .into_iter()
        .map(|link| (link.entity, link.referenced_by.len()))
        .collect();
    all.sort();
    assert_eq!(
        all,
        [
            ("Ghost".to_owned(), 3),
            ("Orphan".to_owned(), 1),
            ("ZFS".to_owned(), 2),
            ("ghost".to_owned(), 1),
            ("m.2 Drive".to_owned(), 1),
            ("rivethub".to_owned(), 1),
            ("zfs".to_owned(), 2),
        ]
    );
    Ok(())
}

struct FixtureJob {
    priority: i16,
    minutes_ago: i32,
    attempts: i16,
    max_attempts: i16,
    locked_minutes_ago: Option<i32>,
    queue: Option<&'static str>,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn job_claims_follow_the_previous_priority_order_and_reclaim_stale_locks(
    pool: PgPool,
) -> TestResult {
    common::install_graphile(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let names: Vec<String> = queue
        .task_names(WorkerRole::Embedder)
        .iter()
        .map(ToString::to_string)
        .collect();
    let jobs = [
        FixtureJob {
            priority: 0,
            minutes_ago: 30,
            attempts: 0,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: None,
        },
        FixtureJob {
            priority: 0,
            minutes_ago: 30,
            attempts: 0,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: None,
        },
        FixtureJob {
            priority: 0,
            minutes_ago: 50,
            attempts: 1,
            max_attempts: 25,
            locked_minutes_ago: Some(300),
            queue: None,
        },
        FixtureJob {
            priority: 0,
            minutes_ago: 40,
            attempts: 1,
            max_attempts: 25,
            locked_minutes_ago: Some(10),
            queue: None,
        },
        FixtureJob {
            priority: -1,
            minutes_ago: 5,
            attempts: 0,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: None,
        },
        FixtureJob {
            priority: 0,
            minutes_ago: 20,
            attempts: 25,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: None,
        },
        FixtureJob {
            priority: 0,
            minutes_ago: 10,
            attempts: 3,
            max_attempts: 3,
            locked_minutes_ago: Some(300),
            queue: None,
        },
        FixtureJob {
            priority: 1,
            minutes_ago: 60,
            attempts: 0,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: Some("serial"),
        },
        FixtureJob {
            priority: 0,
            minutes_ago: -60,
            attempts: 0,
            max_attempts: 25,
            locked_minutes_ago: None,
            queue: None,
        },
        FixtureJob {
            priority: 2,
            minutes_ago: 70,
            attempts: 2,
            max_attempts: 25,
            locked_minutes_ago: Some(500),
            queue: Some("serial"),
        },
    ];
    for job in jobs {
        let id: i64 = sqlx::query_scalar(
            "SELECT (graphile_worker.add_job('embed-target', '{}'::json, $1, priority => $2)).id",
        )
        .bind(job.queue)
        .bind(i32::from(job.priority))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "UPDATE graphile_worker._private_jobs
                SET run_at = now() - make_interval(mins => $2),
                    attempts = $3, max_attempts = $4,
                    locked_at = now() - make_interval(mins => $5),
                    locked_by = CASE WHEN $5::int IS NULL THEN NULL ELSE 'elsewhere' END
              WHERE id = $1",
        )
        .bind(id)
        .bind(job.minutes_ago)
        .bind(job.attempts)
        .bind(job.max_attempts)
        .bind(job.locked_minutes_ago)
        .execute(&pool)
        .await?;
    }
    let expected: Vec<i64> = sqlx::query_scalar(PREVIOUS_CLAIM_ORDER)
        .bind(&names)
        .fetch_all(&pool)
        .await?;
    assert_eq!(expected.len(), 6);
    let selector = ClaimSelector::matching(queue.task_names(WorkerRole::Embedder));
    let mut claimed = Vec::new();
    while let Some(job) = queue.claim(&selector).await? {
        claimed.push(i64::from(job.id));
        queue.complete(job.id).await?;
    }
    assert_eq!(claimed, expected);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_message_captured_twice_is_one_hit(pool: PgPool) -> TestResult {
    conversation(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let first = message(
        &pool,
        1,
        "the backup host rebooted after the nightly snapshot finished",
        None,
        None,
    )
    .await?;
    let mut tx = testkit::capture_tx(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
         SELECT $2, conversation_id, agent, channel, role, content, created_at, owner_user_id
           FROM rh_messages WHERE id = $1",
    )
    .bind(first)
    .bind(Uuid::from_u128(2))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    for mode in [SearchMode::Hybrid, SearchMode::Trigram] {
        assert_eq!(
            hit_ids(&memory, &search("backup host", mode, MemoryScope::Messages))
                .await?
                .len(),
            1
        );
    }
    Ok(())
}

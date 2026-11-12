use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore as _;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use memory_postgres::CaptureOptions;
use wire::message::Role;

use sqlx::PgPool;
use sqlx::Row as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn stamp(text: &str) -> Result<chrono::DateTime<chrono::Utc>, chrono::ParseError> {
    Ok(chrono::DateTime::parse_from_rfc3339(text)?.with_timezone(&chrono::Utc))
}

fn store(pool: &PgPool) -> memory_postgres::PostgresMemory {
    memory_postgres::PostgresMemory::new(pool.clone())
}

fn batch(session_key: &str, messages: Vec<CaptureMessage>) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_string(),
        agent: "rivet".to_string(),
        channel: Some("claude-code".to_string()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages,
    }
}

fn message(event_id: &str, role: Role, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_string(),
        role,
        content: content.to_string(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

const MESSAGES: [&str; 10] = [
    "what is on the AcmeKit release checklist this week",
    "the checklist covers changelog review, version bump, and a staging deploy",
    "how long does the rollback drill take on staging",
    "about ten minutes: switch traffic back, then verify the health endpoint",
    "who signs off on the database migration before release",
    "the on-call engineer approves the migration after a dry run",
    "remind me where the deploy logs are kept",
    "deploy logs are written to /home/user/work/AcmeKit/logs/deploy.log",
    "can we automate the version bump",
    "yes, a release script can bump the version and tag the commit",
];

const LEAF_ANSWERS: [&str; 3] = [
    "The team decided to deploy the AcmeKit service with a blue-green rollout. The release branch is cut on Mondays, the deploy script runs from the release branch, and a health check on staging.example.com gates the traffic switch. Rollback is a traffic switch back to the previous colour.",
    "The AcmeKit release checklist was reviewed: changelog, version bump and a staging deploy, with the database migration signed off by the on-call engineer after a dry run before every release goes out.",
    "Deploy logs live under the AcmeKit logs directory, and the team agreed to automate the version bump with a release script that also tags the commit, removing a manual step from the checklist entirely.",
];

const BRANCH_ANSWER: &str = "Branch summary: several deploy sessions for AcmeKit settled on blue-green releases with a health-check gate.";

const OLDER_AUTOMATION_MESSAGE: &str =
    "we can automate the version bump; the release script for AcmeKit was proposed last quarter";

async fn seed(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let memory = store(pool);
    for (index, text) in MESSAGES.iter().enumerate() {
        let batch = batch(
            "claude-code:seed",
            vec![message(
                &format!("m{index}"),
                if index.is_multiple_of(2) {
                    Role::User
                } else {
                    Role::Assistant
                },
                text,
            )],
        );
        memory
            .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
            .await?;
    }
    let mut older = message("m-old", Role::Assistant, OLDER_AUTOMATION_MESSAGE);
    older.created_at = Some(stamp("2026-01-01T10:00:00Z")?);
    let older = batch("claude-code:seed", vec![older]);
    memory
        .capture_batch(&older, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let mut tx = testkit::capture_tx(pool).await?;
    let conversation: sqlx::types::Uuid = sqlx::query_scalar(
        "SELECT id FROM rh_conversations WHERE session_key = 'claude-code:seed'",
    )
    .fetch_one(&mut *tx)
    .await?;
    for (kind, content) in LEAF_ANSWERS
        .iter()
        .map(|c| ("leaf", *c))
        .chain([("branch", BRANCH_ANSWER)])
    {
        sqlx::query(
            "INSERT INTO rh_summaries (conversation_id, depth, content, kind, message_count, owner_user_id)
             VALUES ($1, $2, $3, $4, 3, 'owner')",
        )
        .bind(conversation)
        .bind(i32::from(kind == "branch"))
        .bind(content)
        .bind(kind)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn contents(results: &[kernel::memory::MemorySearchResult]) -> Vec<String> {
    results.iter().map(|hit| hit.content.clone()).collect()
}

fn query(text: &str, limit: u32) -> SearchQuery {
    SearchQuery {
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
        text: text.to_string(),
        mode: SearchMode::Fts,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(limit), 20, 1..=50),
    }
}

fn browse_filter() -> BrowseFilter {
    BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: false,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(50), 50, 1..=200),
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn append_is_idempotent_per_event_id(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let entry = |content: &str, event: &str| MemoryEntry {
        provenance: None,
        id: None,
        session_id: "claude-code:port".to_string(),
        agent: "rivet".to_string(),
        channel: "claude-code".to_string(),
        role: Role::User,
        content: content.to_string(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: Some(entry_metadata(event)),
        created_at: None,
    };
    let first = memory
        .append(
            &testkit::owner()?,
            vec![entry("first entry", "e1"), entry("second entry", "e2")],
        )
        .await?;
    assert_eq!(first, 2);
    let replay = memory
        .append(
            &testkit::owner()?,
            vec![entry("first entry", "e1"), entry("third entry", "e3")],
        )
        .await?;
    assert_eq!(replay, 1, "the replayed event id is skipped");

    let conversation: sqlx::types::Uuid = sqlx::query_scalar(
        "SELECT id FROM rh_conversations WHERE session_key = 'claude-code:port'",
    )
    .fetch_one(&pool)
    .await?;
    let mut filter = browse_filter();
    filter.conversation_id = Some(conversation);
    let entries = memory.browse(filter).await?;
    let mut names: Vec<&str> = entries.iter().map(|entry| entry.content.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["first entry", "second entry", "third entry"]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_capture_port_skips_resent_events_and_defaults_the_channel(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let mut sent = batch(
        "claude-code:resend",
        vec![
            message("r1", Role::User, "first"),
            message("r2", Role::Assistant, "second"),
        ],
    );
    sent.channel = None;
    sent.title = Some("Resent".to_owned());
    let first = memory
        .capture(sent.clone(), &testkit::owner()?, None)
        .await?;
    let second = memory.capture(sent, &testkit::owner()?, None).await?;
    assert_eq!((first.inserted, first.skipped), (2, 0));
    assert_eq!((second.inserted, second.skipped), (0, 2));
    let row = sqlx::query(
        "select c.channel, c.title, count(m.id) as messages from rh_conversations c
           join rh_messages m on m.conversation_id = c.id
          where c.session_key = 'claude-code:resend' group by c.channel, c.title",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(row.try_get::<String, _>("channel")?, "unknown");
    assert_eq!(
        row.try_get::<Option<String>, _>("title")?.as_deref(),
        Some("Resent")
    );
    assert_eq!(row.try_get::<i64, _>("messages")?, 2);
    Ok(())
}

fn entry_metadata(event: &str) -> wire::JsonObject {
    #[derive(serde::Serialize)]
    struct EntryMetadata<'a> {
        event_id: &'a str,
    }
    serde_json::from_value(
        serde_json::to_value(EntryMetadata { event_id: event }).unwrap_or_default(),
    )
    .unwrap_or_default()
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn search_ranks_like_the_recorded_cases(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = store(&pool);

    let deploy = memory.search(query("deploy", 10)).await?;
    let deploy_contents = contents(&deploy.results);
    assert!(deploy_contents.contains(&MESSAGES[6].to_string()));
    assert!(deploy_contents.contains(&LEAF_ANSWERS[0].to_string()));
    assert!(deploy_contents.contains(&BRANCH_ANSWER.to_string()));
    let deploy_message = deploy
        .results
        .iter()
        .find(|hit| hit.content == MESSAGES[6])
        .ok_or("the deploy message is missing from the hits")?;
    assert_eq!(
        (
            deploy_message.source,
            deploy_message.role,
            deploy_message.agent.as_deref(),
            deploy_message.session_id.as_deref()
        ),
        (
            MemoryHitSource::Message,
            Some(Role::User),
            Some("rivet"),
            Some("claude-code:seed")
        ),
        "a message hit carries its role, agent and session"
    );
    let deploy_summary = deploy
        .results
        .iter()
        .find(|hit| hit.content == BRANCH_ANSWER)
        .ok_or("the branch summary is missing from the hits")?;
    assert_eq!(
        (
            deploy_summary.source,
            deploy_summary.role,
            deploy_summary.agent.as_deref(),
            deploy_summary.session_id.as_deref()
        ),
        (
            MemoryHitSource::Summary,
            None,
            None,
            Some("claude-code:seed")
        ),
        "a summary hit carries its session but no role or agent"
    );

    let staging = memory.search(query("staging database", 5)).await?;
    assert!(
        contents(&staging.results).contains(&LEAF_ANSWERS[1].to_string()),
        "the checklist summary covers both staging and the database migration"
    );

    let rollback = memory.search(query("rollback drill", 5)).await?;
    assert!(contents(&rollback.results).contains(&MESSAGES[2].to_string()));

    let blue_green = memory.search(query("blue-green", 5)).await?;
    assert!(contents(&blue_green.results).contains(&LEAF_ANSWERS[0].to_string()));

    let unbalanced = memory.search(query("\"unbalanced AND (", 5)).await?;
    assert!(unbalanced.results.is_empty(), "fts operators stay quoted");

    let bumped = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let row = sqlx::query(
                "SELECT count(*)::bigint AS n FROM rh_message_access WHERE access_count > 0",
            )
            .fetch_one(&pool)
            .await?;
            let count: i64 = row.try_get("n")?;
            if count > 0 {
                return Ok::<i64, sqlx::Error>(count);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    assert!(
        bumped > 0,
        "search records access counts off the request path"
    );
    let rewritten: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM rh_messages WHERE access_count > 0")
            .fetch_one(&pool)
            .await?;
    assert_eq!(rewritten, 0, "a search hit no longer rewrites its message");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn search_honours_mode_scope_agent_tag_and_limit(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = store(&pool);

    let hybrid = memory
        .search({
            let mut hybrid = query("deploy", 20);
            hybrid.mode = SearchMode::Hybrid;
            hybrid
        })
        .await?;
    assert!(
        !hybrid.results.is_empty(),
        "hybrid mode still ranks the seeded data"
    );

    let short = batch(
        "claude-code:trigram",
        vec![message("t1", Role::User, "deploy checklist")],
    );
    memory
        .capture_batch(&short, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let trigram = memory
        .search(SearchQuery {
            mode: SearchMode::Trigram,
            ..query("Deploy Checkl", 20)
        })
        .await?;
    assert!(
        contents(&trigram.results).contains(&"deploy checklist".to_string()),
        "the trigram arm matches a case-insensitive substring"
    );

    let regex = memory
        .search(SearchQuery {
            mode: SearchMode::Regex,
            ..query("rollback drill", 20)
        })
        .await?;
    assert!(contents(&regex.results).contains(&MESSAGES[2].to_string()));

    let mut summaries_only = query("deploy", 20);
    summaries_only.scope = MemoryScope::Summaries;
    let summaries = memory.search(summaries_only).await?;
    assert_ne!(
        summaries.results.len(),
        0,
        "the summaries scope finds the seeded summaries"
    );
    assert!(
        summaries.results
            .iter()
            .all(|hit| LEAF_ANSWERS.contains(&hit.content.as_str())
                || hit.content == BRANCH_ANSWER),
        "the summaries scope returns summaries only"
    );

    let mut other_agent = query("deploy", 20);
    other_agent.agent = Some("someone-else".to_string());
    assert!(
        memory.search(other_agent).await?.results.is_empty(),
        "the agent filter excludes other agents"
    );

    let mut limited = query("deploy", 2);
    limited.mode = SearchMode::Fts;
    let hits = memory.search(limited).await?;
    assert!(hits.results.len() <= 2, "the limit bounds the result count");

    sqlx::query(
        "INSERT INTO rh_tags (entity_type, entity_id, key, value, display, source, state)
         SELECT 'conversation', id, 'project', 'acmekit', 'AcmeKit', 'rule', 'accepted'
           FROM rh_conversations WHERE session_key = 'claude-code:seed'",
    )
    .execute(&pool)
    .await?;
    let mut tagged = query("deploy", 20);
    tagged.mode = SearchMode::Hybrid;
    tagged.tag = Some(kernel::memory::TagLiteral {
        key: "project".to_string(),
        value: "acmekit".to_string(),
    });
    tagged.limit = Limit::clamped(Some(20), 20, 1..=50);
    let filtered = memory.search(tagged.clone()).await?;
    assert!(
        contents(&filtered.results).contains(&MESSAGES[1].to_string()),
        "the tag filter restricts the search to tagged conversations"
    );
    tagged.tag = Some(kernel::memory::TagLiteral {
        key: "project".to_string(),
        value: "unrelated".to_string(),
    });
    assert!(
        memory.search(tagged).await?.results.is_empty(),
        "an unknown tag value returns no hits"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn stats_get_full_and_context(pool: PgPool) -> TestResult {
    seed(&pool).await?;
    let memory = store(&pool);

    let stats = memory.stats().await?;
    assert_eq!(stats.conversations, 1);
    assert_eq!(stats.messages, 11);
    assert_eq!(stats.summaries, 4);
    assert_eq!(stats.recent_sessions.len(), 1);
    assert_eq!(
        stats
            .recent_sessions
            .first()
            .map(|session| session.messages)
            .unwrap_or_default(),
        11
    );

    let mut filter = browse_filter();
    filter.limit = Limit::clamped(Some(3), 50, 1..=200);
    filter.order = MemoryBrowseOrder::Desc;
    let browse = memory.browse(filter).await?;
    assert_eq!(browse.len(), 3);

    let full = memory
        .get_full(
            browse
                .first()
                .and_then(|entry| entry.id.as_deref())
                .unwrap_or_default(),
        )
        .await?;
    assert_eq!(
        full.as_ref().map(|entry| entry.content.as_str()),
        browse.first().map(|entry| entry.content.as_str())
    );
    assert!(
        memory
            .get_full("00000000-0000-4000-8000-000000000000")
            .await?
            .is_none()
    );

    let context = memory
        .context("claude-code:seed", chrono::Utc::now())
        .await?;
    assert!(context.contains("## Recent"), "{context}");
    assert!(context.contains("## Relevant Context"), "{context}");
    assert!(
        context.contains("[rivet/assistant"),
        "message hits render their agent and role: {context}"
    );
    assert!(
        context.contains("[summary/leaf"),
        "summary hits render from their kind, not a role: {context}"
    );
    assert!(!context.contains("## Wiki"), "no wiki rows exist");

    let empty = memory
        .context("claude-code:heartbeat:x", chrono::Utc::now())
        .await?;
    assert_eq!(empty, "");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn browse_honours_every_filter_field(pool: PgPool) -> TestResult {
    let memory = store(&pool);
    let mut b1 = message("b1", Role::User, "older user prompt");
    b1.created_at = Some(stamp("2026-01-01T10:00:00Z")?);
    let mut b2 = message("b2", Role::Assistant, "assistant reply");
    b2.created_at = Some(stamp("2026-01-01T10:00:01Z")?);
    let mut b3 = message("b3", Role::Tool, "[tool call] Grep");
    b3.tool_name = Some("Grep".to_string());
    b3.tool_result = Some("match found".to_string());
    b3.created_at = Some(stamp("2026-01-01T10:00:02Z")?);
    let mut b4 = message("b4", Role::User, "newer user prompt");
    b4.created_at = Some(stamp("2026-01-02T10:00:00Z")?);
    let batch = batch("claude-code:browse", vec![b1, b2, b3, b4]);
    memory
        .capture_batch(&batch, &CaptureOptions::owned_by(&testkit::owner()?))
        .await?;
    let conversation: sqlx::types::Uuid = sqlx::query_scalar(
        "SELECT id FROM rh_conversations WHERE session_key = 'claude-code:browse'",
    )
    .fetch_one(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rh_tags (entity_type, entity_id, key, value, display, source, state)
         VALUES ('conversation', $1, 'project', 'browsekit', 'BrowseKit', 'rule', 'accepted')",
    )
    .bind(conversation)
    .execute(&pool)
    .await?;

    let base = || BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: false,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(50), 50, 1..=200),
    };

    let all = memory.browse(base()).await?;
    assert_eq!(all.len(), 3, "tool rows stay hidden behind include_tools");

    let mut with_tools = base();
    with_tools.include_tools = true;
    assert_eq!(memory.browse(with_tools.clone()).await?.len(), 4);

    with_tools.role = Some(Role::Tool);
    let tools = memory.browse(with_tools).await?;
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools.first().and_then(|entry| entry.tool_name.as_deref()),
        Some("Grep")
    );

    let mut tool_named = base();
    tool_named.include_tools = true;
    tool_named.tool_name = Some("Grep".to_string());
    assert_eq!(memory.browse(tool_named).await?.len(), 1);

    let mut conversation_only = base();
    conversation_only.include_tools = true;
    conversation_only.conversation_id = Some(conversation);
    assert_eq!(memory.browse(conversation_only).await?.len(), 4);

    let mut other_agent = base();
    other_agent.include_tools = true;
    other_agent.agent = Some("other".to_string());
    assert_eq!(
        memory.browse(other_agent).await?.len(),
        0,
        "another agent's rows are excluded"
    );

    let mut tagged = base();
    tagged.tag = Some(kernel::memory::TagLiteral {
        key: "project".to_string(),
        value: "browsekit".to_string(),
    });
    assert_eq!(memory.browse(tagged).await?.len(), 3);

    let mut ascending = base();
    ascending.include_tools = true;
    ascending.order = MemoryBrowseOrder::Asc;
    ascending.limit = Limit::clamped(Some(2), 50, 1..=200);
    let oldest_first = memory.browse(ascending).await?;
    let stamps: Vec<i64> = oldest_first
        .iter()
        .map(|entry| {
            entry
                .created_at
                .map_or(i64::MAX, |at| at.timestamp_millis())
        })
        .collect();
    let ordered = stamps.windows(2).all(|pair| {
        pair.first()
            .zip(pair.get(1))
            .is_some_and(|(first, second)| first <= second)
    });
    assert!(ordered, "ascending order is honoured: {stamps:?}");

    let mut since = base();
    since.include_tools = true;
    since.since = Some("2026-01-02T00:00:00Z".parse()?);
    let newer = memory.browse(since).await?;
    assert_eq!(newer.len(), 1);

    let mut before = base();
    before.include_tools = true;
    before.before = Some("2026-01-02T00:00:00Z".parse()?);
    assert_eq!(memory.browse(before).await?.len(), 3);

    let mut window = base();
    window.window = Some(kernel::memory::BrowseWindow::Last14d);
    let recent = memory
        .browse(window.resolve_window("2026-10-15T12:00:00Z".parse()?, &chrono::Utc))
        .await?;
    assert_eq!(
        recent.len(),
        0,
        "the January seed data falls outside the window"
    );
    Ok(())
}

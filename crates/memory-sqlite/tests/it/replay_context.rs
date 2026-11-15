use crate::common;

use common::Scenario;
use common::ScenarioFile;
use common::Step;
use common::TestResult;
use common::execute;
use common::expect_logs;
use common::fresh_store;
use common::instant;
use common::recording;
use kernel::memory::MemoryStore;
use memory_sqlite::SqliteStore;
use rusqlite::params;

const CHECKLIST: &str = "recent, relevant, dedup, heartbeat excluded, budget";
const WIKI: &str = "wiki section and wiki failure";
const CHECKLIST_SESSION: &str = "cli:checklist";
const HELPER_SESSION: &str = "cli:helper";
#[tokio::test]
async fn context_replay_matches_the_recording() -> TestResult {
    let file: ScenarioFile = recording("context.json")?;
    replay_checklist(find(&file, CHECKLIST)?).await?;
    replay_wiki(find(&file, WIKI)?).await?;
    Ok(())
}

async fn replay_checklist(scenario: &Scenario) -> TestResult {
    let fresh = fresh_store()?;
    seed_checklist(&fresh.store, scenario).await?;
    expect_context(
        &fresh.store,
        step(scenario, "context deploy checklist")?,
        CHECKLIST_SESSION,
    )
    .await?;
    expect_context(
        &fresh.store,
        step(scenario, "context helper")?,
        HELPER_SESSION,
    )
    .await?;
    expect_context(
        &fresh.store,
        step(scenario, "context unknown agent")?,
        "cli:missing",
    )
    .await?;
    expect_logs(
        &scenario.logs,
        &fresh.store.logs(),
        CHECKLIST,
        &mut common::Ids::default(),
    )?;
    Ok(())
}

async fn replay_wiki(scenario: &Scenario) -> TestResult {
    let fresh = fresh_store()?;
    let shown = step(scenario, "context with wiki")?;
    let text = shown_text(shown)?;
    seed_wiki_context(&fresh.store, &text).await?;
    expect_context(&fresh.store, shown, CHECKLIST_SESSION).await?;
    drop_wiki_index(&fresh.store).await?;
    expect_context(
        &fresh.store,
        step(scenario, "context with failing wiki search")?,
        CHECKLIST_SESSION,
    )
    .await?;
    expect_logs(
        &scenario.logs,
        &fresh.store.logs(),
        WIKI,
        &mut common::Ids::default(),
    )?;
    Ok(())
}

async fn expect_context(store: &SqliteStore, step: &Step, session: &str) -> TestResult {
    let now = instant(&step.at)?;
    let actual = store.context(session, now).await?;
    let expected = shown_text(step)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{}\nactual:\n{actual}\nexpected:\n{expected}", step.label).into())
    }
}

fn shown_text(step: &Step) -> TestResult<String> {
    match step.result.as_ref() {
        Some(testkit::Json::String(text)) => Ok(text.clone()),
        Some(other) => Err(format!("{} result is {other}", step.label).into()),
        None => Err(format!("{} has no result", step.label).into()),
    }
}

async fn seed_checklist(store: &SqliteStore, scenario: &Scenario) -> TestResult {
    let shown = shown_text(step(scenario, "context deploy checklist")?)?;
    let long = shown
        .lines()
        .find(|line| line.starts_with("[assistant] deploy long"))
        .ok_or("recorded long line")?
        .trim_start_matches("[assistant] ")
        .to_owned();
    conversation(
        store,
        "c-check",
        CHECKLIST_SESSION,
        "rivet",
        "2026-10-10T12:00:00.000Z",
    )
    .await?;
    conversation(
        store,
        "c-heart",
        "heartbeat:checklist",
        "rivet",
        "2026-10-10T12:00:00.000Z",
    )
    .await?;
    conversation(
        store,
        "c-help",
        HELPER_SESSION,
        "helper",
        "2026-10-10T12:06:00.000Z",
    )
    .await?;
    message(
        store,
        "m-heart",
        "c-heart",
        body(
            "rivet",
            "system",
            "heartbeat deploy checklist ok",
            "2026-10-10T12:00:00.000Z",
        ),
    )
    .await?;
    seed_numbered(store).await?;
    message(
        store,
        "m-long",
        "c-check",
        body("rivet", "assistant", &long, "2026-10-10T12:06:40.000Z"),
    )
    .await?;
    message(
        store,
        "m-helper",
        "c-help",
        body(
            "helper",
            "user",
            "helper deploy checklist message",
            "2026-10-10T12:06:50.000Z",
        ),
    )
    .await?;
    Ok(())
}

async fn seed_numbered(store: &SqliteStore) -> TestResult {
    for (index, role, at) in [
        (0, "user", "2026-10-10T12:01:00.000Z"),
        (1, "assistant", "2026-10-10T12:02:00.000Z"),
        (2, "user", "2026-10-10T12:03:00.000Z"),
        (3, "assistant", "2026-10-10T12:04:00.000Z"),
        (4, "user", "2026-10-10T12:05:00.000Z"),
        (5, "assistant", "2026-10-10T12:06:00.000Z"),
        (6, "user", "2026-10-10T12:06:30.000Z"),
    ] {
        message(
            store,
            &format!("m-{index}"),
            "c-check",
            body(
                "rivet",
                role,
                &format!("message number {index} about the AcmeKit deploy checklist"),
                at,
            ),
        )
        .await?;
    }
    Ok(())
}

async fn seed_wiki_context(store: &SqliteStore, recorded: &str) -> TestResult {
    conversation(
        store,
        "c-wiki",
        CHECKLIST_SESSION,
        "rivet",
        "2026-10-10T12:00:00.000Z",
    )
    .await?;
    for (index, role, at) in [
        (0, "user", "2026-10-10T12:00:00.000Z"),
        (1, "assistant", "2026-10-10T12:01:00.000Z"),
        (2, "user", "2026-10-10T12:02:00.000Z"),
    ] {
        message(
            store,
            &format!("w-{index}"),
            "c-wiki",
            body(
                "rivet",
                role,
                &format!("message number {index} about the AcmeKit staging database restore"),
                at,
            ),
        )
        .await?;
    }
    let section = between(
        recorded,
        "## Wiki (curated state)\n",
        "\n## Relevant Context",
    )?;
    let pages = wiki_pages(section)?;
    let query = "message number 2 about the AcmeKit staging database restore";
    for (index, page) in pages.iter().enumerate() {
        let search = if index == 0 {
            query.to_owned()
        } else {
            format!("{query} {}", page.lead)
        };
        wiki_topic(store, page, &search).await?;
    }
    Ok(())
}

struct WikiPage {
    slug: String,
    title: String,
    lead: String,
    article: String,
}

fn wiki_pages(section: &str) -> TestResult<Vec<WikiPage>> {
    let mut pages = Vec::new();
    let mut rest = section;
    while rest.starts_with("**") {
        let (block, next) = match rest[2..].find("\n**") {
            Some(at) => (&rest[..at + 2], &rest[at + 3..]),
            None => (rest, ""),
        };
        pages.push(wiki_page(block)?);
        rest = next;
    }
    if pages.len() == 2 {
        Ok(pages)
    } else {
        Err(format!("wiki section has {} blocks", pages.len()).into())
    }
}

fn wiki_page(block: &str) -> TestResult<WikiPage> {
    let (header, body) = block.split_once('\n').ok_or("wiki block header")?;
    let title = header
        .trim_start_matches("**")
        .split_once("**")
        .ok_or("wiki title")?
        .0
        .to_owned();
    let slug = header
        .split_once("(wiki:")
        .ok_or("wiki slug")?
        .1
        .trim_end_matches(')')
        .to_owned();
    let (lead, article) = match body.split_once('\n') {
        Some((lead, article)) => (lead.to_owned(), article.to_owned()),
        None => (body.to_owned(), String::new()),
    };
    Ok(WikiPage {
        slug,
        title,
        lead,
        article,
    })
}

async fn wiki_topic(store: &SqliteStore, page: &WikiPage, search: &str) -> TestResult {
    let slug = page.slug.clone();
    let title = page.title.clone();
    let lead = page.lead.clone();
    let article = page.article.clone();
    let search = search.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_wiki_topics \
                    (slug, title, aliases, tags, entities, related, current_state, article, \
                     search_text, created_at, updated_at) \
                 VALUES (?1, ?2, '[]', '[]', '[]', '[]', ?3, ?4, ?5, ?6, ?6)",
                params![
                    slug,
                    title,
                    lead,
                    article,
                    search,
                    "2026-10-10T12:00:00.000Z"
                ],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn drop_wiki_index(store: &SqliteStore) -> TestResult {
    execute(store, "DROP TABLE ros_wiki_topics_fts").await
}

async fn conversation(
    store: &SqliteStore,
    id: &str,
    session: &str,
    agent: &str,
    at: &str,
) -> TestResult {
    let id = id.to_owned();
    let session = session.to_owned();
    let agent = agent.to_owned();
    let at = at.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, settings, active, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'cli', '{}', 1, ?4, ?4)",
                params![id, session, agent, at],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

struct MessageRow<'a> {
    agent: &'a str,
    role: &'a str,
    content: &'a str,
    at: &'a str,
}

fn body<'a>(agent: &'a str, role: &'a str, content: &'a str, at: &'a str) -> MessageRow<'a> {
    MessageRow {
        agent,
        role,
        content,
        at,
    }
}

async fn message(
    store: &SqliteStore,
    id: &str,
    conversation_id: &str,
    body: MessageRow<'_>,
) -> TestResult {
    let id = id.to_owned();
    let conversation_id = conversation_id.to_owned();
    let agent = body.agent.to_owned();
    let role = body.role.to_owned();
    let content = body.content.to_owned();
    let at = body.at.to_owned();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_messages \
                    (id, conversation_id, agent, channel, role, content, metadata, created_at) \
                 VALUES (?1, ?2, ?3, 'cli', ?4, ?5, '{}', ?6)",
                params![id, conversation_id, agent, role, content, at],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> TestResult<&'a str> {
    let rest = text.split_once(start).ok_or("missing wiki section")?.1;
    Ok(rest.split_once(end).ok_or("missing relevant section")?.0)
}

fn find<'a>(file: &'a ScenarioFile, name: &str) -> TestResult<&'a Scenario> {
    file.scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| format!("missing scenario {name}").into())
}

fn step<'a>(scenario: &'a Scenario, label: &str) -> TestResult<&'a Step> {
    scenario
        .steps
        .iter()
        .find(|step| step.label == label)
        .ok_or_else(|| format!("missing step {label}").into())
}

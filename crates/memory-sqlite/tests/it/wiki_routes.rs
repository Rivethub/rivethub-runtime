use kernel::transcript::SessionLister;
use kernel::wiki::builder::ExtractWiki;
use memory_sqlite::SqliteStore;
use tempfile::TempDir;
use testkit::topic_pages::PageStores;
use testkit::wiki_routes;
use testkit::wiki_routes::RouteStores;
use uuid::Uuid;

use crate::common::TestResult;
use crate::common::execute;

const CONVERSATION: &str = "00000000-0000-4000-8000-0000000000c1";
const SUMMARY: &str = "00000000-0000-4000-8000-0000000000d1";
const EARLIER_CONVERSATION: &str = "00000000-0000-4000-8000-0000000000c2";
const EARLIER_SUMMARY: &str = "00000000-0000-4000-8000-0000000000d2";
const SUBAGENT_CONVERSATION: &str = "00000000-0000-4000-8000-0000000000c3";
const SUBAGENT_SUMMARY: &str = "00000000-0000-4000-8000-0000000000d3";
const SECOND_SUMMARY: &str = "00000000-0000-4000-8000-0000000000d4";
const THIRD_SUMMARY: &str = "00000000-0000-4000-8000-0000000000d5";

async fn seeded() -> TestResult<(TempDir, SqliteStore, ExtractWiki)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    execute(
        &store,
        &format!(
            "INSERT INTO ros_conversations (id, session_key, agent, title, created_at, updated_at, owner_user_id)
             VALUES ('{CONVERSATION}', '{}', 'rivet', '{}', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z', 'owner');
             INSERT INTO ros_summaries (id, conversation_id, content, created_at)
             VALUES ('{SUMMARY}', '{CONVERSATION}', 'AcmeKit notes.', '2026-10-10T12:00:00.000Z');",
            wiki_routes::SESSION_KEY,
            wiki_routes::SESSION_TITLE
        ),
    )
    .await?;
    let job = ExtractWiki {
        summary_id: Uuid::parse_str(SUMMARY)?,
        conversation_id: Some(Uuid::parse_str(CONVERSATION)?),
    };
    Ok((directory, store, job))
}

fn stores(store: &SqliteStore) -> RouteStores<'_> {
    stores_in(store, &wiki_routes::NoParents)
}

fn stores_in<'a>(store: &'a SqliteStore, lineage: &'a dyn SessionLister) -> RouteStores<'a> {
    RouteStores {
        pages: PageStores {
            topics: store,
            pages: store,
            decisions: store,
            wiki: store,
            queue: store,
        },
        wiki: store,
        filing: store,
        lineage,
    }
}

#[tokio::test]
async fn a_placed_session_writes_its_topic_page_and_no_other_topics_page() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_placed_session_writes_its_topic_page_and_no_other_topics_page(
        stores(&store),
        &job,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn the_topic_page_is_cited_without_a_proposal() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::the_topic_page_is_cited_without_a_proposal(stores(&store), &job).await?;
    Ok(())
}

#[tokio::test]
async fn a_second_session_naming_an_entity_gives_it_its_own_page() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_second_session_naming_an_entity_gives_it_its_own_page(stores(&store), &job)
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_entity_page_names_the_sessions_that_cite_it() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    testkit::wiki_mentions::an_entity_page_names_the_sessions_that_cite_it(stores(&store), &job)
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_mention_held_before_its_entity_existed_still_lands() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    execute(
        &store,
        &format!(
            "INSERT INTO ros_conversations (id, session_key, agent, title, created_at, updated_at, owner_user_id)
             VALUES ('{EARLIER_CONVERSATION}', '{}', 'rivet', 'Lab rack', '2026-10-10T11:00:00.000Z', '2026-10-10T11:00:00.000Z', 'owner');
             INSERT INTO ros_summaries (id, conversation_id, content, created_at)
             VALUES ('{EARLIER_SUMMARY}', '{EARLIER_CONVERSATION}', 'Lab notes.', '2026-10-10T11:00:00.000Z');",
            testkit::wiki_mentions::EARLIER_KEY
        ),
    )
    .await?;
    let earlier = ExtractWiki {
        summary_id: Uuid::parse_str(EARLIER_SUMMARY)?,
        conversation_id: Some(Uuid::parse_str(EARLIER_CONVERSATION)?),
    };
    testkit::wiki_mentions::a_mention_held_before_its_entity_existed_still_lands(
        stores(&store),
        &job,
        &earlier,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn an_entity_type_is_found_by_its_whole_path() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    testkit::wiki_mentions::an_entity_type_is_found_by_its_whole_path(stores(&store), &job).await?;
    Ok(())
}

#[tokio::test]
async fn an_entity_goes_by_its_names_never_by_a_description() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    testkit::wiki_mentions::an_entity_goes_by_its_names_never_by_a_description(
        stores(&store),
        &job,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn an_unfiled_session_waits_until_it_is_filed() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::an_unfiled_session_waits_until_it_is_filed(stores(&store), &job).await?;
    Ok(())
}

#[tokio::test]
async fn a_child_session_waits_for_its_parent() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_child_session_waits_for_its_parent(
        stores_in(&store, &testkit::wiki_lineage::CHILD),
        &job,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_nested_child_s_notes_land_on_its_filed_ancestor_s_topic() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    testkit::wiki_lineage::a_child_s_notes_land_on_its_filed_ancestor_s_topic(
        stores_in(&store, &testkit::wiki_lineage::GRANDCHILD),
        &job,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_subagent_key_s_notes_land_on_its_parent_s_topic() -> TestResult {
    let (_directory, store, _job) = seeded().await?;
    execute(
        &store,
        &format!(
            "INSERT INTO ros_conversations (id, session_key, agent, title, created_at, updated_at, owner_user_id)
             VALUES ('{SUBAGENT_CONVERSATION}', '{}', 'rivet', 'Billing check', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z', 'owner');
             INSERT INTO ros_summaries (id, conversation_id, content, created_at)
             VALUES ('{SUBAGENT_SUMMARY}', '{SUBAGENT_CONVERSATION}', 'Billing notes.', '2026-10-10T12:00:00.000Z');",
            testkit::wiki_lineage::SUBAGENT_KEY
        ),
    )
    .await?;
    let subagent = ExtractWiki {
        summary_id: Uuid::parse_str(SUBAGENT_SUMMARY)?,
        conversation_id: Some(Uuid::parse_str(SUBAGENT_CONVERSATION)?),
    };
    testkit::wiki_lineage::a_child_s_notes_land_on_its_filed_ancestor_s_topic(
        stores(&store),
        &subagent,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_moved_session_takes_its_notes_along() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_moved_session_takes_its_notes_along(stores(&store), &job).await?;
    Ok(())
}

#[tokio::test]
async fn a_deleted_entity_is_not_recreated() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_deleted_entity_is_not_recreated(stores(&store), &job).await?;
    Ok(())
}

#[tokio::test]
async fn a_sub_topic_summary_lands_in_its_section() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    wiki_routes::a_sub_topic_summary_lands_in_its_section(stores(&store), &job).await?;
    Ok(())
}

async fn second(store: &SqliteStore) -> TestResult<ExtractWiki> {
    summary(store, SECOND_SUMMARY, "2026-10-10T13:00:00.000Z").await
}

async fn summary(store: &SqliteStore, id: &str, at: &str) -> TestResult<ExtractWiki> {
    execute(
        store,
        &format!(
            "INSERT INTO ros_summaries (id, conversation_id, content, created_at)
             VALUES ('{id}', '{CONVERSATION}', 'More AcmeKit notes.', '{at}');"
        ),
    )
    .await?;
    Ok(ExtractWiki {
        summary_id: Uuid::parse_str(id)?,
        conversation_id: Some(Uuid::parse_str(CONVERSATION)?),
    })
}

#[tokio::test]
async fn a_sub_topic_rewrite_keeps_its_lead() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    let next = second(&store).await?;
    testkit::wiki_bounded::a_sub_topic_rewrite_keeps_its_lead(stores(&store), &job, &next).await?;
    Ok(())
}

#[tokio::test]
async fn concurrent_rewrites_of_one_page_keep_both() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    let next = second(&store).await?;
    testkit::wiki_bounded::concurrent_rewrites_of_one_page_keep_both(stores(&store), &job, &next)
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_entity_mention_is_merged_into_its_page() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    let next = second(&store).await?;
    testkit::wiki_bounded::an_entity_mention_is_merged_into_its_page(stores(&store), &job, &next)
        .await?;
    Ok(())
}

#[tokio::test]
async fn updates_keep_the_lead_and_replace_only_the_currently_sentence() -> TestResult {
    let (_directory, store, job) = seeded().await?;
    let next = second(&store).await?;
    let last = summary(&store, THIRD_SUMMARY, "2026-10-10T14:00:00.000Z").await?;
    testkit::wiki_bounded::updates_keep_the_lead_and_replace_only_the_currently_sentence(
        stores(&store),
        [&job, &next, &last],
    )
    .await?;
    Ok(())
}

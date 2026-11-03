use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicStore;
use memory_sqlite::SqliteStore;
use tempfile::TempDir;
use testkit::topic_pages;
use testkit::topic_pages::PageStores;
use testkit::topics;

use crate::common::TestResult;

fn store() -> TestResult<(TempDir, SqliteStore)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = SqliteStore::open(&path)?;
    Ok((directory, store))
}

fn pages(store: &SqliteStore) -> PageStores<'_> {
    PageStores {
        topics: store,
        pages: store,
        decisions: store,
        wiki: store,
        queue: store,
    }
}

#[tokio::test]
async fn a_new_root_topic_gets_an_empty_bound_page() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::a_new_root_topic_gets_an_empty_bound_page(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn a_rename_keeps_the_page_slug_and_follows_the_name() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::a_rename_keeps_the_page_slug_and_follows_the_name(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn a_merge_folds_the_page_and_redirects_its_slug() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::a_merge_folds_the_page_and_redirects_its_slug(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn sub_topics_are_sections_of_the_root_page() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::sub_topics_are_sections_of_the_root_page(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn syncing_twice_changes_nothing() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::syncing_twice_changes_nothing(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn topics_are_created_found_and_renamed() -> TestResult {
    let (_directory, store) = store()?;
    topics::topics_are_created_found_and_renamed(&store).await?;
    Ok(())
}

#[tokio::test]
async fn slugs_and_aliases_are_unique_across_topics() -> TestResult {
    let (_directory, store) = store()?;
    topics::slugs_and_aliases_are_unique_across_topics(&store).await?;
    Ok(())
}

#[tokio::test]
async fn topics_nest_up_to_four_levels() -> TestResult {
    let (_directory, store) = store()?;
    topics::topics_nest_up_to_four_levels(&store).await?;
    Ok(())
}

#[tokio::test]
async fn placements_keep_your_choice() -> TestResult {
    let (_directory, store) = store()?;
    topics::placements_keep_your_choice(&store).await?;
    Ok(())
}

#[tokio::test]
async fn topics_close_and_sessions_are_marked_as_history() -> TestResult {
    let (_directory, store) = store()?;
    topics::topics_close_and_sessions_are_marked_as_history(&store).await?;
    Ok(())
}

#[tokio::test]
async fn merges_move_placements_aliases_and_slugs() -> TestResult {
    let (_directory, store) = store()?;
    topics::merges_move_placements_aliases_and_slugs(&store).await?;
    Ok(())
}

#[tokio::test]
async fn redirects_follow_a_second_merge() -> TestResult {
    let (_directory, store) = store()?;
    topics::redirects_follow_a_second_merge(&store).await?;
    Ok(())
}

#[tokio::test]
async fn suggestions_hold_until_the_session_is_placed() -> TestResult {
    let (_directory, store) = store()?;
    topics::suggestions_hold_until_the_session_is_placed(&store).await?;
    Ok(())
}

#[tokio::test]
async fn startup_context_is_kept_per_session() -> TestResult {
    let (_directory, store) = store()?;
    topics::startup_context_is_kept_per_session(&store).await?;
    Ok(())
}

#[tokio::test]
async fn listings_count_sessions_and_take_last_activity_from_conversations() -> TestResult {
    let (_directory, store) = store()?;
    let busy = store.create(topics::new_topic("Busy", &[], None)?).await?;
    let quiet = store.create(topics::new_topic("Quiet", &[], None)?).await?;
    store.create(topics::new_topic("Empty", &[], None)?).await?;
    let first = topics::session("00000000-0000-4000-8000-000000000011")?;
    let second = topics::session("00000000-0000-4000-8000-000000000012")?;
    let uncaptured = topics::session("00000000-0000-4000-8000-000000000013")?;
    for (session, topic) in [
        (&first, busy.id),
        (&second, busy.id),
        (&uncaptured, quiet.id),
    ] {
        store
            .place(Placement {
                session: session.clone(),
                topic,
                by: PlacementSource::You,
                at: topics::at(1),
                owner: testkit::owner().ok(),
            })
            .await?;
    }
    let keys = [first.to_string(), second.to_string()];
    store
        .with_conn(move |conn| {
            for (index, (key, agent, updated)) in [
                (&keys[0], "opus", "2026-10-02T09:00:00.000Z"),
                (&keys[0], "grok", "2026-10-03T09:00:00.000Z"),
                (&keys[1], "opus", "2026-10-01T09:00:00.000Z"),
            ]
            .into_iter()
            .enumerate()
            {
                conn.execute(
                    "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    rusqlite::params![
                        format!("00000000-0000-4000-8000-00000000010{index}"),
                        key,
                        agent,
                        updated
                    ],
                )
                .map_err(|source| memory_sqlite::SqliteError::Query {
                    operation: "seed conversations",
                    source,
                })?;
            }
            Ok(())
        })
        .await?;
    let listed = testkit::topics::work_topics(&store).await?;
    let summary: Vec<(&str, u64, Option<String>)> = listed
        .iter()
        .map(|listing| {
            (
                listing.topic.name.as_str(),
                listing.session_count,
                listing.last_active.map(|at| at.to_rfc3339()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("Busy", 2, Some("2026-10-03T09:00:00+00:00".to_owned())),
            ("Empty", 0, None),
            ("Quiet", 1, None),
        ]
    );
    let sessions = store.sessions(busy.id).await?;
    assert_eq!(
        sessions
            .iter()
            .map(|placed| placed.placement.session.clone())
            .collect::<Vec<_>>(),
        [first, second]
    );
    Ok(())
}

#[tokio::test]
async fn decisions_are_the_topic_page_decisions_section() -> TestResult {
    let (_directory, store) = store()?;
    testkit::decisions::decisions_are_the_topic_page_decisions_section(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn a_deleted_topic_leaves_its_sessions_forming_and_its_page_unbound() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::a_deleted_topic_leaves_its_sessions_forming_and_its_page_unbound(pages(&store))
        .await?;
    Ok(())
}

#[tokio::test]
async fn conversations_file_with_titles_and_corrections() -> TestResult {
    let (_directory, store) = store()?;
    testkit::filing::conversations_file_with_titles_and_corrections(&store).await?;
    Ok(())
}

#[tokio::test]
async fn a_task_files_under_the_conversation_that_started_it() -> TestResult {
    let (_directory, store) = store()?;
    testkit::filing::a_task_files_under_the_conversation_that_started_it(&store).await?;
    Ok(())
}

#[tokio::test]
async fn editing_an_unwritten_entity_writes_its_page() -> TestResult {
    let (_directory, store) = store()?;
    topic_pages::editing_an_unwritten_entity_writes_its_page(pages(&store)).await?;
    Ok(())
}

#[tokio::test]
async fn proposals_are_decided_and_rejections_remembered() -> TestResult {
    let (_directory, store) = store()?;
    testkit::tidy::proposals_are_decided_and_rejections_remembered(&store).await?;
    Ok(())
}

#[tokio::test]
async fn an_approval_that_stopped_part_way_finishes_on_retry() -> TestResult {
    let (_directory, store) = store()?;
    testkit::tidy::an_approval_that_stopped_part_way_finishes_on_retry(&store).await?;
    Ok(())
}

#[tokio::test]
async fn old_names_stay_findable_and_a_change_made_moot_is_skipped() -> TestResult {
    let (_directory, store) = store()?;
    testkit::tidy::old_names_stay_findable_and_a_change_made_moot_is_skipped(&store).await?;
    Ok(())
}

#[tokio::test]
async fn samples_show_each_session_s_current_title() -> TestResult {
    let (_directory, store) = store()?;
    testkit::tidy::samples_show_each_session_s_current_title(&store).await?;
    Ok(())
}

#[tokio::test]
async fn upkeep_counts_filings_entities_and_finished_jobs() -> TestResult {
    let (_directory, store) = store()?;
    testkit::tidy::upkeep_counts_filings_entities_and_finished_jobs(&store, &store).await?;
    Ok(())
}

#[tokio::test]
async fn every_topic_s_filing_reads_at_once() -> TestResult {
    let (_directory, store) = store()?;
    testkit::filing_bulk::every_topic_s_filing_reads_at_once(&store).await?;
    Ok(())
}

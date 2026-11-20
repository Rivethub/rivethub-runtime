use crate::search_support;
use jiff::Timestamp;

use std::collections::BTreeSet;

use kernel::memory::MemoryScope;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use memory_sqlite::SqliteStore;
use search_support::TestResult;
use search_support::failed;
use search_support::query;
use search_support::store;
use wire::message::Role;

const USER: &str = "00000000-0000-4000-8000-000000000201";
const ASSISTANT: &str = "00000000-0000-4000-8000-000000000202";
const BASH: &str = "00000000-0000-4000-8000-000000000203";
const READ: &str = "00000000-0000-4000-8000-000000000204";
const HELPER_BASH: &str = "00000000-0000-4000-8000-000000000205";
const SUMMARY: &str = "00000000-0000-4000-8000-400000000201";
const TEXT: &str = "backup disk";

async fn seed(store: &SqliteStore) -> TestResult {
    store
        .with_conn(|conn| {
            let at = "2026-10-10T12:00:00.000Z";
            for (id, agent) in [("ops", "rivet"), ("side", "helper")] {
                conn.execute(
                    "INSERT INTO ros_conversations(id, session_key, agent, channel, created_at, updated_at) VALUES (?1, ?1, ?2, 'cli', ?3, ?3)",
                    rusqlite::params![id, agent, at],
                )
                .map_err(failed)?;
            }
            for (id, conversation, agent, role, tool, content) in [
                (USER, "ops", "rivet", "user", None, "the nightly backup disk is full and the job stopped"),
                (ASSISTANT, "ops", "rivet", "assistant", None, "prune the oldest snapshots on the backup disk before the next run"),
                (BASH, "ops", "rivet", "tool", Some("Bash"), "df -h reports the backup disk at one hundred percent used"),
                (READ, "ops", "rivet", "tool", Some("Read"), "backup.yaml keeps thirty snapshots on the backup disk volume"),
                (HELPER_BASH, "side", "helper", "tool", Some("Bash"), "du -sh lists the backup disk usage per snapshot folder"),
            ] {
                conn.execute(
                    "INSERT INTO ros_messages(id, conversation_id, role, content, agent, channel, created_at, tool_name) VALUES (?1, ?2, ?3, ?4, ?5, 'cli', ?6, ?7)",
                    rusqlite::params![id, conversation, role, content, agent, at, tool],
                )
                .map_err(failed)?;
            }
            conn.execute(
                "INSERT INTO ros_summaries(id, content, kind, created_at, conversation_id) VALUES (?1, 'The nightly backup disk filled up until the old snapshots were pruned.', 'leaf', ?2, 'ops')",
                rusqlite::params![SUMMARY, at],
            )
            .map_err(failed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn clock() -> TestResult<Timestamp> {
    Ok("2026-10-11T12:00:00Z".parse()?)
}

async fn hits(store: &SqliteStore, query: SearchQuery) -> TestResult<BTreeSet<String>> {
    let outcome = store
        .search_at(query, clock()?, &jiff::tz::TimeZone::UTC)
        .await?;
    Ok(outcome.results.into_iter().map(|hit| hit.id).collect())
}

fn ids(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|id| (*id).to_owned()).collect()
}

const MODES: [SearchMode; 3] = [SearchMode::Fts, SearchMode::Regex, SearchMode::Hybrid];

#[tokio::test]
async fn a_role_filter_keeps_only_messages_with_that_role_and_the_summaries() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    for mode in MODES {
        let mut search = query(TEXT, mode);
        search.role = Some(Role::User);
        assert_eq!(hits(&store, search).await?, ids(&[USER, SUMMARY]), "{mode}");
        let mut search = query(TEXT, mode);
        search.role = Some(Role::Tool);
        search.scope = MemoryScope::Messages;
        assert_eq!(
            hits(&store, search).await?,
            ids(&[BASH, READ, HELPER_BASH]),
            "{mode}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_tool_filter_keeps_only_that_tool_and_combines_with_role_and_agent() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    for mode in MODES {
        let mut search = query(TEXT, mode);
        search.tool_name = Some("Bash".to_owned());
        assert_eq!(
            hits(&store, search).await?,
            ids(&[BASH, HELPER_BASH, SUMMARY]),
            "{mode}"
        );
        let mut search = query(TEXT, mode);
        search.tool_name = Some("Bash".to_owned());
        search.agent = Some("helper".to_owned());
        search.scope = MemoryScope::Messages;
        assert_eq!(hits(&store, search).await?, ids(&[HELPER_BASH]), "{mode}");
        let mut search = query(TEXT, mode);
        search.tool_name = Some("Read".to_owned());
        search.role = Some(Role::Assistant);
        assert_eq!(hits(&store, search).await?, ids(&[SUMMARY]), "{mode}");
    }
    Ok(())
}

#[tokio::test]
async fn without_filters_every_matching_row_is_found() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    for mode in MODES {
        assert_eq!(
            hits(&store, query(TEXT, mode)).await?,
            ids(&[USER, ASSISTANT, BASH, READ, HELPER_BASH, SUMMARY]),
            "{mode}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn excluded_roles_drop_messages_and_summary_hits_name_their_session() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    for mode in MODES {
        let mut search = query(TEXT, mode);
        search.exclude_roles = vec![Role::Tool, Role::System];
        assert_eq!(
            hits(&store, search).await?,
            ids(&[USER, ASSISTANT, SUMMARY]),
            "{mode}"
        );
        let mut search = query(TEXT, mode);
        search.scope = MemoryScope::Summaries;
        let outcome = store
            .search_at(search, clock()?, &jiff::tz::TimeZone::UTC)
            .await?;
        let sessions: Vec<Option<&str>> = outcome
            .results
            .iter()
            .map(|hit| hit.session_id.as_deref())
            .collect();
        assert_eq!(sessions, vec![Some("ops")], "{mode}");
    }
    Ok(())
}

#[tokio::test]
async fn time_filters_compare_instants_at_the_requested_precision() -> TestResult {
    let (_dir, store) = store(None)?;
    seed(&store).await?;
    crate::common::execute(
        &store,
        &format!(
            "UPDATE ros_messages SET created_at = '2026-10-10T13:00:00.123100Z' WHERE id = '{USER}';
             UPDATE ros_messages SET created_at = '2026-10-10T14:00:00.123900+01:00' WHERE id = '{ASSISTANT}';"
        ),
    )
    .await?;
    for mode in MODES {
        let mut search = query(TEXT, mode);
        search.scope = MemoryScope::Messages;
        search.since = Some("2026-10-10T13:00:00.123100Z".parse()?);
        search.before = Some("2026-10-10T13:00:00.123900Z".parse()?);
        assert_eq!(hits(&store, search).await?, ids(&[USER]), "{mode}");
    }
    Ok(())
}

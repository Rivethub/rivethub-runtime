use std::path::PathBuf;

use kernel::memory::MemoryStore;
use kernel::topics::TopicPageStore;
use kernel::wiki::entity_pass::EntityStore;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::WikiSlug;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use tempfile::TempDir;
use testkit::wiki_files::CONVERSATION;
use testkit::wiki_files::FAILED_MENTION;
use testkit::wiki_files::FAILED_SENTENCE;
use wire::time::TimestampFormat as _;

use crate::common::TestResult;

struct Opened {
    _dir: TempDir,
    database: PathBuf,
    store: SqliteStore,
    wiki: PathBuf,
}

#[tokio::test]
async fn a_failed_merge_removal_is_finished_later() -> TestResult {
    let opened = open()?;
    let when = "2026-10-10T20:00:00Z".parse()?;
    let left = testkit::wiki_files::publish_entity(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b1",
        "keepbox",
        "Keep this box.",
        "Noted keepbox.",
        "2026-10-10T18:00:00Z",
    )
    .await?;
    let right = testkit::wiki_files::publish_entity(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b2",
        "dropbox",
        "Drop this box.",
        "Noted dropbox.",
        "2026-10-10T19:00:00Z",
    )
    .await?;
    let file = opened.wiki.join("topics").join("dropbox.md");
    {
        let _guard = wiki_files::wiki_writer::fail_git(&opened.wiki, "rm");
        let merged = opened.store.merge_entities(left.id, right.id, when).await?;
        let merged = merged.ok_or("merge produced nothing")?;
        assert!(merged.performed);
    }
    assert!(file.exists(), "a failed removal left no file to retry");
    opened.store.merge_entities(left.id, right.id, when).await?;
    assert!(!file.exists(), "a later merge left the absorbed file");
    Ok(())
}

#[tokio::test]
async fn topic_removal_restores_citations_when_the_delete_stops() -> TestResult {
    let opened = open()?;
    opened
        .store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at)
                 VALUES ('gone-page', 'Gone', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
                [],
            )
            .map_err(query)?;
            conn.execute(
                "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
                 VALUES ('gone-page', 's1', 'leaf', 'note', '2026-10-10T12:00:00.000Z')",
                [],
            )
            .map_err(query)?;
            conn.execute_batch(
                "CREATE TRIGGER stop_topic_delete BEFORE DELETE ON ros_wiki_topics
                 BEGIN
                   SELECT RAISE(ABORT, 'stop');
                 END;",
            )
            .map_err(query)?;
            Ok(())
        })
        .await?;
    let removed = opened
        .store
        .remove_page(&WikiSlug::try_from("gone-page".to_owned())?)
        .await;
    assert!(
        removed.is_err(),
        "the aborted delete was reported as success"
    );
    let (topics, citations) = opened
        .store
        .with_conn(|conn| {
            let topics: i64 = conn
                .query_row(
                    "SELECT count(*) FROM ros_wiki_topics WHERE slug = 'gone-page'",
                    [],
                    |row| row.get(0),
                )
                .map_err(query)?;
            let citations: i64 = conn
                .query_row(
                    "SELECT count(*) FROM ros_wiki_citations WHERE topic_slug = 'gone-page'",
                    [],
                    |row| row.get(0),
                )
                .map_err(query)?;
            Ok((topics, citations))
        })
        .await?;
    assert_eq!(topics, 1);
    assert_eq!(citations, 1);
    Ok(())
}

#[tokio::test]
async fn an_expired_lease_does_not_replace_the_page() -> TestResult {
    let opened = open()?;
    let view = testkit::wiki_files::publish_entity(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b3",
        "leasebox",
        "Lease prose.",
        "Noted leasebox.",
        "2026-10-10T18:00:00Z",
    )
    .await?;
    let path = opened.wiki.join("topics").join("leasebox.md");
    let bytes = std::fs::read(&path)?;
    let mut lease = opened.store.lease_entity(view.id).await?;
    let id = view.id.to_string();
    opened
        .store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE rh_entity_leases SET expires_at = unixepoch() - 10 WHERE entity_id = ?1",
                [id],
            )
            .map_err(query)?;
            Ok(())
        })
        .await?;
    let when = "2026-10-10T19:00:00Z".parse()?;
    let summary = "00000000-0000-4000-8000-0000000000b4".parse()?;
    testkit::wiki_files::write_leaf(
        &opened.store,
        "00000000-0000-4000-8000-0000000000b4",
        "Expired prose.",
        "2026-10-10T19:00:00Z",
    )
    .await?;
    let failed = lease
        .apply(
            &StoredRewrite {
                summary: "Expired prose.".to_owned(),
                article: "This article must not land.".to_owned(),
                facts: vec![PreparedFact {
                    attribute: "place".to_owned(),
                    value: "bench".to_owned(),
                    as_of: when,
                    status: FactState::Current,
                    summary_id: summary,
                    conversation_id: Some(CONVERSATION.parse()?),
                }],
                history: Some(PreparedHistory {
                    summary_id: summary,
                    conversation_id: Some(CONVERSATION.parse()?),
                    session_on: when.utc_date(),
                    sentence: "Expired rewrite.".to_owned(),
                }),
                owner: testkit::OWNER.to_owned(),
                session_at: when,
                session_id: summary,
                evidence: "Expired rewrite.".to_owned(),
            },
            when,
        )
        .await;
    assert!(failed.is_err(), "an expired lease still published the page");
    assert_eq!(std::fs::read(&path)?, bytes);
    let text = String::from_utf8(bytes)?;
    assert!(!text.contains("must not land"), "{text}");
    Ok(())
}

#[tokio::test]
async fn archive_parts_are_numeric_and_a_missing_root_is_empty() -> TestResult {
    let opened = open()?;
    let missing = opened.wiki.join("does-not-exist");
    let store = SqliteStore::open(&opened.database)?.with_wiki_directory(missing);
    let slug = WikiSlug::try_from("kept".to_owned())?;
    assert_eq!(store.wiki_archive(&slug, None).await?, None);
    let directory = opened.wiki.join("archive").join("kept");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("2026-10.10.md"), "part ten\n")?;
    std::fs::write(directory.join("2026-10.2.md"), "part two\n")?;
    let text = opened
        .store
        .wiki_archive(&slug, None)
        .await?
        .ok_or("archive was empty")?;
    let two = text.find("part two").ok_or("part two missing")?;
    let ten = text.find("part ten").ok_or("part ten missing")?;
    assert!(two < ten, "{text}");
    Ok(())
}

#[tokio::test]
async fn git_failure_leaves_history_and_the_marker() -> TestResult {
    let opened = open()?;
    testkit::wiki_files::commit_failure_keeps_rows(&opened.store, &opened.wiki, || {
        wiki_files::wiki_writer::fail_git(&opened.wiki, "commit")
    })
    .await?;
    testkit::wiki_files::stale_publish_keeps_the_file(&opened.store, &opened.wiki).await?;
    let (rewrites, mentions) = opened
        .store
        .with_conn(|conn| {
            let rewrites: i64 = conn
                .query_row(
                    "SELECT count(*) FROM rh_entity_history WHERE sentence = ?1",
                    params![FAILED_SENTENCE],
                    |row| row.get(0),
                )
                .map_err(query)?;
            let mentions: i64 = conn
                .query_row(
                    "SELECT count(*) FROM rh_entity_history WHERE sentence = ?1",
                    params![FAILED_MENTION],
                    |row| row.get(0),
                )
                .map_err(query)?;
            Ok((rewrites, mentions))
        })
        .await?;
    assert_eq!(rewrites, 0);
    assert_eq!(mentions, 0);
    Ok(())
}

fn open() -> TestResult<Opened> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let wiki = dir.path().join("wiki");
    std::fs::create_dir(&wiki)?;
    let conn = rusqlite::Connection::open(&database)?;
    conn.execute(
        "INSERT INTO ros_conversations
            (id, session_key, agent, title, created_at, updated_at, owner_user_id)
         VALUES (?1, 'claude-code:page-rows', 'rivet', 'rows', '2026-10-10T12:00:00.000Z',
                 '2026-10-10T12:00:00.000Z', 'owner')",
        [CONVERSATION],
    )?;
    let store = SqliteStore::open(&database)?.with_wiki_directory(wiki.clone());
    Ok(Opened {
        _dir: dir,
        database,
        store,
        wiki,
    })
}

fn query(source: rusqlite::Error) -> SqliteError {
    SqliteError::Query {
        operation: "page row test",
        source,
    }
}

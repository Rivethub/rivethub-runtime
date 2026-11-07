use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;
use rusqlite::params;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CONVERSATION: &str = "0199e2a07b3c7a008000000000000001";
const TASK: &str = "0199e2a07b3c7a008000000000000002";
const FIRST_MESSAGE: &str = "0199e2a07b3c7a008001000000000001";
const SECOND_MESSAGE: &str = "0199E2A07B3C7A00800100000000000A";
const LEAF: &str = "0199e2a07b3c7a008002000000000001";
const BRANCH: &str = "0199e2a07b3c7a008002000000000002";
const CONVERSATION_TAG: &str = "0199e2a07b3c7a008003000000000001";
const SUMMARY_TAG: &str = "0199e2a07b3c7a008003000000000002";
const DASHED_TAG: &str = "0199e2a0-7b3c-7a00-8003-000000000003";
const HEX_SESSION: &str = "abcdefabcdefabcdefabcdefabcdefab";
const NOT_HEX: &str = "0199e2a07b3c7a00800300000000000g";
const AT: &str = "2026-10-10T12:00:00.000Z";

fn dashed(id: &str) -> String {
    let id = id.to_ascii_lowercase();
    format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    )
}

fn seed(path: &Path) -> TestResult {
    migrate::sqlite(path)?;
    let conn = Connection::open(path)?;
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, task_id, created_at, updated_at)
         VALUES (?1, ?2, 'rivet', ?3, ?4, ?4)",
        params![CONVERSATION, HEX_SESSION, TASK, AT],
    )?;
    for (id, content) in [
        (FIRST_MESSAGE, "ledger one"),
        (SECOND_MESSAGE, "ledger two"),
    ] {
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'rivet', 'test', 'user', ?3, ?4)",
            params![id, CONVERSATION, content, AT],
        )?;
    }
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, depth, kind, content, created_at)
         VALUES (?1, ?2, 1, 'branch', 'ledger branch', ?3)",
        params![BRANCH, CONVERSATION, AT],
    )?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, parent_id, depth, kind, content, created_at)
         VALUES (?1, ?2, ?3, 0, 'leaf', 'ledger leaf', ?4)",
        params![LEAF, CONVERSATION, BRANCH, AT],
    )?;
    for (ordinal, message) in [FIRST_MESSAGE, SECOND_MESSAGE].into_iter().enumerate() {
        conn.execute(
            "INSERT INTO ros_summary_sources (summary_id, message_id, ordinal) VALUES (?1, ?2, ?3)",
            params![LEAF, message, i64::try_from(ordinal)?],
        )?;
    }
    for (id, entity_type, entity_id, value) in [
        (CONVERSATION_TAG, "conversation", CONVERSATION, "ledger"),
        (SUMMARY_TAG, "summary", LEAF, "ledger"),
        (DASHED_TAG, "conversation", &dashed(CONVERSATION), "ledger"),
        (NOT_HEX, "conversation", NOT_HEX, "other"),
    ] {
        conn.execute(
            "INSERT INTO ros_tags (id, entity_type, entity_id, key, value, source, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'topic', ?4, 'user', 'accepted', ?5, ?5)",
            params![id, entity_type, entity_id, value, AT],
        )?;
    }
    seed_references(&conn)?;
    conn.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE);
         PRAGMA journal_mode = DELETE;
         PRAGMA user_version = 9;",
    )?;
    Ok(())
}

fn seed_references(conn: &Connection) -> TestResult {
    conn.execute(
        "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at) VALUES ('ledger', 'Ledger', ?1, ?1)",
        [AT],
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, created_at)
         VALUES ('ledger', 'summary', ?1, ?2, ?3)",
        params![LEAF, CONVERSATION, AT],
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, extracted_at)
         VALUES (?1, 'done', 4, ?2)",
        params![LEAF, AT],
    )?;
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, cited_at) VALUES ('ledger', ?1, ?2)",
        params![LEAF, AT],
    )?;
    for (id, task, key, payload) in [
        (
            "job-embed",
            "embed-target",
            format!("embed-ros_messages-{FIRST_MESSAGE}"),
            format!(r#"{{"targetTable":"ros_messages","targetId":"{FIRST_MESSAGE}"}}"#),
        ),
        (
            "job-tags",
            "suggest-tags",
            format!("tags-{LEAF}"),
            format!(r#"{{"summaryId":"{LEAF}","conversationId":"{CONVERSATION}"}}"#),
        ),
        (
            "job-compact-undashed",
            "summarize",
            format!("compact-{CONVERSATION}"),
            format!(r#"{{"conversationId":"{CONVERSATION}"}}"#),
        ),
        (
            "job-compact-dashed",
            "summarize",
            format!("compact-{}", dashed(CONVERSATION)),
            format!(r#"{{"conversationId":"{}"}}"#, dashed(CONVERSATION)),
        ),
    ] {
        conn.execute(
            "INSERT INTO ros_jobs (id, task, job_key, payload, run_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5)",
            params![id, task, key, payload, AT],
        )?;
    }
    Ok(())
}

fn column(conn: &Connection, sql: &str) -> TestResult<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|value| value.unwrap_or_else(|| "NULL".to_owned()))
        .collect())
}

fn snapshot(path: &Path) -> TestResult<Vec<Vec<String>>> {
    let conn = Connection::open(path)?;
    let queries = [
        "SELECT id || ' ' || task_id || ' ' || session_key FROM ros_conversations",
        "SELECT id || ' ' || conversation_id FROM ros_messages ORDER BY content",
        "SELECT id || ' ' || conversation_id || ' ' || coalesce(parent_id, '-') FROM ros_summaries ORDER BY depth",
        "SELECT summary_id || ' ' || message_id FROM ros_summary_sources ORDER BY ordinal",
        "SELECT id || ' ' || entity_id FROM ros_tags ORDER BY entity_type, value",
        "SELECT source_id || ' ' || conversation_id FROM ros_wiki_provenance",
        "SELECT summary_id FROM ros_wiki_extractions",
        "SELECT summary_id FROM ros_wiki_citations",
        "SELECT job_key || ' ' || payload FROM ros_jobs ORDER BY job_key",
        "SELECT id FROM ros_messages_fts WHERE ros_messages_fts MATCH 'ledger' ORDER BY id",
        "SELECT id FROM rh_messages_trigram WHERE rh_messages_trigram MATCH 'ledger' ORDER BY id",
        "SELECT id FROM ros_summaries_fts WHERE ros_summaries_fts MATCH 'ledger' ORDER BY id",
        "SELECT id FROM rh_summaries_trigram WHERE rh_summaries_trigram MATCH 'ledger' ORDER BY id",
        "SELECT name FROM sqlite_master WHERE type = 'trigger' AND name IN ('ros_messages_au', 'ros_summaries_au') ORDER BY name",
    ];
    queries.iter().map(|sql| column(&conn, sql)).collect()
}

fn backup(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".v9.bak");
    PathBuf::from(name)
}

#[test]
fn undashed_ids_are_canonical_in_every_table_that_holds_or_references_them() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    seed(&path)?;
    migrate::sqlite(&path)?;
    let conversation = dashed(CONVERSATION);
    let first = dashed(FIRST_MESSAGE);
    let second = dashed(SECOND_MESSAGE);
    let leaf = dashed(LEAF);
    let branch = dashed(BRANCH);
    let mut messages = vec![first.clone(), second.clone()];
    messages.sort();
    let mut summaries = vec![branch.clone(), leaf.clone()];
    summaries.sort();
    let expected = vec![
        vec![format!("{conversation} {} {HEX_SESSION}", dashed(TASK))],
        vec![
            format!("{first} {conversation}"),
            format!("{second} {conversation}"),
        ],
        vec![
            format!("{leaf} {conversation} {branch}"),
            format!("{branch} {conversation} -"),
        ],
        vec![format!("{leaf} {first}"), format!("{leaf} {second}")],
        vec![
            format!("{DASHED_TAG} {conversation}"),
            format!("{NOT_HEX} {NOT_HEX}"),
            format!("{} {leaf}", dashed(SUMMARY_TAG)),
        ],
        vec![format!("{leaf} {conversation}")],
        vec![leaf.clone()],
        vec![leaf.clone()],
        vec![
            format!(r#"compact-{conversation} {{"conversationId":"{conversation}"}}"#),
            format!(
                r#"embed-ros_messages-{first} {{"targetTable":"ros_messages","targetId":"{first}"}}"#
            ),
            format!(r#"tags-{leaf} {{"summaryId":"{leaf}","conversationId":"{conversation}"}}"#),
        ],
        messages.clone(),
        messages,
        summaries.clone(),
        summaries,
        vec!["ros_messages_au".to_owned(), "ros_summaries_au".to_owned()],
    ];
    let migrated = snapshot(&path)?;
    assert_eq!(migrated, expected);
    let conn = Connection::open(&path)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 34);
    let violations: i64 =
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    assert_eq!(violations, 0);
    drop(conn);

    std::fs::remove_file(backup(&path))?;
    Connection::open(&path)?.execute_batch("PRAGMA user_version = 9;")?;
    migrate::sqlite(&path)?;
    assert_eq!(snapshot(&path)?, migrated, "a second pass changes nothing");
    Ok(())
}

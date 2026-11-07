use std::collections::HashSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStoreError;
use kernel::memory::budget::push_within_budget;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use wire::message::Role;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::stored_role;
use crate::search::fts_or;
use crate::time;

const MAX_TOKENS: usize = 4_000;
const RECENT_ROWS: usize = 5;
const RELEVANT_LIMIT: usize = 10;
const LINE_CAP: usize = 500;
const DEDUP_PREFIX: usize = 300;
const WIKI_LEAD_CAP: usize = 1_200;
const WIKI_ARTICLE_CAP: usize = 800;
const WIKI_TOPICS: usize = 3;
const WIKI_SKIPPED: &str = "[memory.sqlite] wiki context skipped: index unavailable";

struct Line {
    id: String,
    role: Role,
    agent: String,
    content: String,
}

impl SqliteStore {
    pub(crate) async fn context_for(
        &self,
        session: &str,
        now: DateTime<Utc>,
    ) -> Result<String, MemoryStoreError> {
        let session = session.to_owned();
        let (text, wiki_failed) = self
            .with_conn(move |conn| render(conn, &session, now))
            .await
            .map_err(MemoryStoreError::from)?;
        if wiki_failed {
            self.record_log(WIKI_SKIPPED);
        }
        Ok(text)
    }
}

fn render(
    conn: &Connection,
    session: &str,
    now: DateTime<Utc>,
) -> Result<(String, bool), SqliteError> {
    let Some(agent) = agent_of(conn, session)? else {
        return Ok((String::new(), false));
    };
    let stamp = time::stamp(now);
    let mut budget = MAX_TOKENS;
    let mut dedup = HashSet::new();
    let recent = recent_lines(conn, &agent, &stamp, &mut budget, &mut dedup)?;
    let (wiki, wiki_failed) = match wiki_blocks(conn, session, &stamp) {
        Ok(blocks) => (kept_blocks(&mut budget, &mut dedup, blocks), false),
        Err(error) => {
            tracing::debug!("{error}");
            (Vec::new(), true)
        }
    };
    let relevant = relevant_lines(conn, &agent, &stamp, &recent, &mut budget, &mut dedup)?;
    let mut sections = vec![section("## Recent", &body(&recent))];
    if !wiki.is_empty() {
        sections.push(section("## Wiki (curated state)", &wiki));
    }
    sections.push(section("## Relevant Context", &body(&relevant)));
    Ok((format!("\n{}", sections.join("\n\n")), wiki_failed))
}

fn body(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|line| line.content.clone()).collect()
}

fn section(title: &str, lines: &[String]) -> String {
    let mut out = title.to_owned();
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

fn recent_lines(
    conn: &Connection,
    agent: &str,
    stamp: &str,
    budget: &mut usize,
    dedup: &mut HashSet<String>,
) -> Result<Vec<Line>, SqliteError> {
    let mut rows = messages(
        conn,
        "SELECT m.id, m.role, m.agent, m.content FROM ros_messages m \
           JOIN ros_conversations c ON c.id = m.conversation_id \
          WHERE c.agent = ?1 AND c.active = 1 AND c.session_key NOT LIKE 'heartbeat:%' \
            AND m.created_at <= ?2 \
            AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key) \
          ORDER BY m.created_at DESC, m.id DESC LIMIT ?3",
        agent,
        stamp,
        RECENT_ROWS,
    )?;
    rows.reverse();
    Ok(take_budget(rows, budget, dedup, false))
}

fn relevant_lines(
    conn: &Connection,
    agent: &str,
    stamp: &str,
    recent: &[Line],
    budget: &mut usize,
    dedup: &mut HashSet<String>,
) -> Result<Vec<Line>, SqliteError> {
    let mut sql = String::from(
        "SELECT m.id, m.role, m.agent, m.content FROM ros_messages m \
           JOIN ros_conversations c ON c.id = m.conversation_id \
          WHERE c.agent = ? AND m.created_at <= ? \
            AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key)",
    );
    let mut params = vec![Value::Text(agent.to_owned()), Value::Text(stamp.to_owned())];
    if !recent.is_empty() {
        sql.push_str(" AND m.id NOT IN (");
        for (index, line) in recent.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            params.push(Value::Text(line.id.clone()));
        }
        sql.push(')');
    }
    sql.push_str(" ORDER BY m.created_at ASC, m.id ASC LIMIT ?");
    params.push(Value::Integer(
        i64::try_from(RELEVANT_LIMIT).map_err(SqliteError::Count)?,
    ));
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("context relevant"))?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), read_line)
        .map_err(SqliteError::query("context relevant"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("context relevant"))?;
    Ok(take_budget(rows, budget, dedup, true))
}

fn messages(
    conn: &Connection,
    sql: &str,
    agent: &str,
    stamp: &str,
    limit: usize,
) -> Result<Vec<Line>, SqliteError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(SqliteError::query("context recent"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(agent.to_owned()),
                Value::Text(stamp.to_owned()),
                Value::Integer(i64::try_from(limit).map_err(SqliteError::Count)?),
            ]),
            read_line,
        )
        .map_err(SqliteError::query("context recent"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("context recent"))?;
    Ok(rows)
}

fn read_line(row: &rusqlite::Row<'_>) -> rusqlite::Result<Line> {
    Ok(Line {
        id: row.get(0)?,
        role: stored_role(row, 1)?,
        agent: row.get(2)?,
        content: row.get(3)?,
    })
}

fn take_budget(
    rows: Vec<Line>,
    budget: &mut usize,
    dedup: &mut HashSet<String>,
    with_agent: bool,
) -> Vec<Line> {
    let mut kept = Vec::new();
    for row in rows {
        let key = char_prefix(&row.content, DEDUP_PREFIX).to_owned();
        if !dedup.insert(key) {
            continue;
        }
        let text = if with_agent {
            format!(
                "[{}/{}] {}",
                row.agent,
                row.role,
                char_prefix(&row.content, LINE_CAP)
            )
        } else {
            format!("[{}] {}", row.role, char_prefix(&row.content, LINE_CAP))
        };
        if !push_within_budget(budget, &text) {
            break;
        }
        kept.push(Line {
            id: row.id,
            role: row.role,
            agent: row.agent,
            content: text,
        });
    }
    kept
}

fn wiki_blocks(
    conn: &Connection,
    session: &str,
    stamp: &str,
) -> Result<Vec<(String, String)>, SqliteError> {
    let Some(query) = latest_user(conn, session, stamp)? else {
        return Ok(Vec::new());
    };
    let Some(pattern) = fts_or(&query) else {
        return Ok(Vec::new());
    };
    let mut statement = conn
        .prepare(
            "SELECT t.title, t.slug, t.current_state, t.article \
               FROM ros_wiki_topics_fts f \
               JOIN ros_wiki_topics t ON t.slug = f.slug \
              WHERE ros_wiki_topics_fts MATCH ?1 \
              ORDER BY f.rank ASC, t.slug ASC \
              LIMIT ?2",
        )
        .map_err(SqliteError::query("wiki context"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(pattern),
                Value::Integer(i64::try_from(WIKI_TOPICS).map_err(SqliteError::Count)?),
            ]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(SqliteError::query("wiki context"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki context"))?;
    Ok(rows
        .into_iter()
        .map(|(title, slug, state, article)| wiki_block(&title, &slug, &state, &article))
        .collect())
}

fn kept_blocks(
    budget: &mut usize,
    dedup: &mut HashSet<String>,
    blocks: Vec<(String, String)>,
) -> Vec<String> {
    let mut kept = Vec::new();
    for (state, block) in blocks {
        dedup.insert(char_prefix(&state, DEDUP_PREFIX).to_owned());
        if !push_within_budget(budget, &block) {
            break;
        }
        kept.push(block);
    }
    kept
}

fn wiki_block(title: &str, slug: &str, state: &str, article: &str) -> (String, String) {
    let mut block = format!("**{title}** (wiki:{slug})");
    let lead = char_prefix(state, WIKI_LEAD_CAP);
    if !lead.trim().is_empty() {
        block.push('\n');
        block.push_str(lead);
    }
    let article = char_prefix(article, WIKI_ARTICLE_CAP);
    if !article.trim().is_empty() {
        block.push('\n');
        block.push_str(article);
    }
    (state.to_owned(), block)
}

fn latest_user(
    conn: &Connection,
    session: &str,
    stamp: &str,
) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        "SELECT m.content FROM ros_messages m \
           JOIN ros_conversations c ON c.id = m.conversation_id \
          WHERE c.session_key = ?1 AND m.role = 'user' AND m.created_at <= ?2 \
          ORDER BY m.created_at DESC, m.id DESC LIMIT 1",
        params_from_iter([
            Value::Text(session.to_owned()),
            Value::Text(stamp.to_owned()),
        ]),
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("context user"))
}

fn agent_of(conn: &Connection, session: &str) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        "SELECT agent FROM ros_conversations WHERE session_key = ?1 ORDER BY id LIMIT 1",
        params_from_iter([Value::Text(session.to_owned())]),
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("context session"))
}

fn char_prefix(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => match text.get(..end) {
            Some(prefix) => prefix,
            None => text,
        },
        None => text,
    }
}

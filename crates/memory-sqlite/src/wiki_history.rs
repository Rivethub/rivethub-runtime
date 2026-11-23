use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::page::WikiHistoryEntry;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

pub(crate) const CAP: usize = 48;

pub(crate) fn load(conn: &Connection, slug: &str) -> Result<Vec<WikiHistoryEntry>, WikiStoreError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT history FROM ros_wiki_topics WHERE slug = ?1",
            params![slug],
            |row| row.get(0),
        )
        .optional()
        .map_err(query)?;
    match text {
        Some(text) => decode(&text),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn with_entry(
    mut history: Vec<WikiHistoryEntry>,
    entry: Option<&WikiHistoryEntry>,
) -> Vec<WikiHistoryEntry> {
    if let Some(entry) = entry
        && !history.contains(entry)
    {
        history.insert(0, entry.clone());
    }
    cap(&mut history);
    history
}

pub(crate) fn merge(parent: &mut Vec<WikiHistoryEntry>, child: Vec<WikiHistoryEntry>) {
    for entry in child {
        if !parent.contains(&entry) {
            parent.push(entry);
        }
    }
    parent.sort_by_key(|entry| std::cmp::Reverse(entry.date));
}

pub(crate) fn cap(history: &mut Vec<WikiHistoryEntry>) {
    while history.len() > CAP {
        history.pop();
    }
}

pub(crate) fn encode(history: &[WikiHistoryEntry]) -> Result<String, WikiStoreError> {
    serde_json::to_string(history).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn decode(text: &str) -> Result<Vec<WikiHistoryEntry>, WikiStoreError> {
    serde_json::from_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn query(source: rusqlite::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(source))
}

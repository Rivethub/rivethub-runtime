use std::path::Path;

use rusqlite::Connection;
use rusqlite::OpenFlags;

use crate::error::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignObject {
    pub kind: ForeignKind,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKind {
    Module,
    Trigger,
}

impl std::fmt::Display for ForeignObject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ForeignKind::Module => write!(formatter, "module {} ({})", self.detail, self.name),
            ForeignKind::Trigger => write!(formatter, "trigger {}", self.name),
        }
    }
}

pub fn sqlite_foreign(path: &Path) -> Result<Vec<ForeignObject>, Error> {
    if !path.try_exists().map_err(|source| Error::Inspect {
        path: path.to_path_buf(),
        source,
    })? {
        return Ok(Vec::new());
    }
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|source| {
            Error::SqliteOpen {
                path: path.to_path_buf(),
                source,
            }
        })?;
    read_foreign(&connection).map_err(Error::SqliteForeign)
}

pub fn foreign_line(objects: &[ForeignObject]) -> String {
    if objects.is_empty() {
        return "foreign: none".to_owned();
    }
    let body = objects
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    format!("foreign: {body}")
}

fn read_foreign(connection: &Connection) -> rusqlite::Result<Vec<ForeignObject>> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_master
          WHERE sql IS NOT NULL AND type IN ('table', 'trigger')
          ORDER BY rowid",
    )?;
    let mut rows = statement.query([])?;
    let mut modules = Vec::new();
    let mut triggers = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let sql: String = row.get(2)?;
        if kind == "table" {
            if let Some(module) = virtual_module(&sql)
                && !bundled(&module)
            {
                modules.push((name, module));
            }
        } else {
            triggers.push((name, sql));
        }
    }
    let mut found = Vec::new();
    for (name, module) in &modules {
        found.push(ForeignObject {
            kind: ForeignKind::Module,
            name: name.clone(),
            detail: module.clone(),
        });
    }
    for (name, sql) in triggers {
        if modules.iter().any(|(table, _)| mentions(&sql, table)) {
            found.push(ForeignObject {
                kind: ForeignKind::Trigger,
                name,
                detail: String::new(),
            });
        }
    }
    Ok(found)
}

fn virtual_module(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let index = lower.find(" using ")?;
    let rest = sql.get(index + " using ".len()..)?.trim_start();
    let end = rest
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(rest.len());
    let name = rest.get(..end)?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

fn bundled(module: &str) -> bool {
    matches!(
        module.to_ascii_lowercase().as_str(),
        "fts3" | "fts4" | "fts5" | "rtree"
    )
}

fn mentions(sql: &str, table: &str) -> bool {
    sql.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|word| word == table)
}

#[cfg(test)]
mod tests {
    use super::bundled;
    use super::sqlite_foreign;
    use crate::schema::sqlite;

    #[test]
    fn a_pristine_file_has_no_foreign_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory.sqlite");
        sqlite(&path).expect("migrate");
        let found = sqlite_foreign(&path).expect("foreign");
        assert_eq!(found, Vec::new());
    }

    #[test]
    fn an_unloaded_virtual_table_and_its_trigger_are_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("shared.sqlite");
        sqlite(&path).expect("migrate");
        let connection = rusqlite::Connection::open(&path).expect("open");
        connection
            .execute_batch(
                "
                CREATE TABLE messages (id INTEGER PRIMARY KEY);
                PRAGMA writable_schema = ON;
                INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
                VALUES (
                    'table',
                    'messages_vec',
                    'messages_vec',
                    0,
                    'CREATE VIRTUAL TABLE messages_vec USING vec0(id float[4])'
                );
                INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
                VALUES (
                    'trigger',
                    'messages_ad',
                    'messages',
                    0,
                    'CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN DELETE FROM messages_vec WHERE id = OLD.id; END'
                );
                PRAGMA writable_schema = OFF;
                ",
            )
            .expect("inject");
        drop(connection);
        let found = sqlite_foreign(&path).expect("foreign");
        let rendered = found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        assert_eq!(rendered, "module vec0 (messages_vec); trigger messages_ad");
        assert!(!bundled("vec0"));
    }

    #[test]
    fn a_missing_file_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let found = sqlite_foreign(&dir.path().join("missing.sqlite")).expect("foreign");
        assert_eq!(found, Vec::new());
    }
}

mod filter;
mod format;

use jiff::Timestamp;
use std::collections::BTreeSet;
use std::io::Write;

use rusqlite::types::Value;
use strum::IntoEnumIterator;
use tokio_util::sync::CancellationToken;

use crate::SqliteError;
use crate::SqliteStore;
use crate::import::ImportTable;
use crate::time::stamp;

pub use kernel::memory::DumpOrigin;
pub use kernel::memory::DumpSource;

impl SqliteStore {
    pub async fn export_pg_dump(
        &self,
        since: Option<Timestamp>,
        origin: DumpOrigin,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, SqliteError> {
        self.with_conn(move |conn| export_dump(conn, since, &origin, &cancellation))
            .await
    }
}

fn export_dump(
    conn: &mut rusqlite::Connection,
    since: Option<Timestamp>,
    origin: &DumpOrigin,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, SqliteError> {
    let mut out = Vec::new();
    write_header(&mut out, origin)?;
    let tx = conn.transaction().map_err(SqliteError::query("export"))?;
    for table in ImportTable::iter() {
        cancelled(cancellation)?;
        if table.dump_columns().is_none() {
            continue;
        }
        write_table(&tx, &mut out, table, since)?;
    }
    out.extend_from_slice(b"-- PostgreSQL database dump complete\n");
    Ok(out)
}

fn write_header(out: &mut Vec<u8>, origin: &DumpOrigin) -> Result<(), SqliteError> {
    if origin.host.is_empty() || origin.host.contains(['\n', '\r']) {
        return Err(SqliteError::Invalid {
            message: "export host is empty or has a line break".into(),
        });
    }
    writeln!(
        out,
        "-- RivetHub memory export\n-- source {}\n-- host {}",
        origin.source, origin.host
    )
    .map_err(|source| SqliteError::Invalid {
        message: source.to_string(),
    })
}

fn write_table(
    conn: &rusqlite::Connection,
    out: &mut Vec<u8>,
    table: ImportTable,
    since: Option<Timestamp>,
) -> Result<(), SqliteError> {
    let Some(canonical) = table.dump_columns() else {
        return Err(SqliteError::Invalid {
            message: format!("{table} is not an export table"),
        });
    };
    let relation = table.compatibility_name();
    ident(&relation)?;
    let columns = existing(conn, &relation, canonical)?;
    if columns.is_empty() {
        return Err(SqliteError::Invalid {
            message: format!("{relation} has no exportable columns"),
        });
    }
    for column in &columns {
        ident(column)?;
    }
    writeln!(
        out,
        "COPY public.{relation} ({}) FROM stdin;",
        columns.join(", ")
    )
    .map_err(|source| SqliteError::Invalid {
        message: source.to_string(),
    })?;
    write_rows(conn, out, table, &relation, &columns, since)?;
    out.extend_from_slice(b"\\.\n");
    Ok(())
}

fn write_rows(
    conn: &rusqlite::Connection,
    out: &mut Vec<u8>,
    table: ImportTable,
    relation: &str,
    columns: &[&str],
    since: Option<Timestamp>,
) -> Result<(), SqliteError> {
    let order = table.dump_order();
    ident_list(order)?;
    let sql = match since {
        Some(_) => format!(
            "SELECT {cols} FROM {relation} WHERE {body} ORDER BY {order}",
            cols = columns.join(", "),
            body = filter::predicate(table, ":since")
        ),
        None => format!(
            "SELECT {cols} FROM {relation} ORDER BY {order}",
            cols = columns.join(", ")
        ),
    };
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("export select"))?;
    let mark = since.map(stamp);
    let mut rows = match &mark {
        Some(mark) => statement
            .query(rusqlite::named_params! { ":since": mark })
            .map_err(SqliteError::query("export select"))?,
        None => statement
            .query([])
            .map_err(SqliteError::query("export select"))?,
    };
    while let Some(row) = rows.next().map_err(SqliteError::query("export select"))? {
        write_row(out, columns, row)?;
    }
    Ok(())
}

fn write_row(
    out: &mut Vec<u8>,
    columns: &[&str],
    row: &rusqlite::Row<'_>,
) -> Result<(), SqliteError> {
    for (index, name) in columns.iter().enumerate() {
        if index > 0 {
            out.push(b'\t');
        }
        let value: Value = row
            .get(index)
            .map_err(SqliteError::query("export select"))?;
        out.extend_from_slice(format::field(name, value)?.as_bytes());
    }
    out.push(b'\n');
    Ok(())
}

fn existing<'a>(
    conn: &rusqlite::Connection,
    relation: &str,
    columns: &'a [&'a str],
) -> Result<Vec<&'a str>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .map_err(SqliteError::query("export columns"))?;
    let known = statement
        .query_map([relation], |row| row.get(0))
        .map_err(SqliteError::query("export columns"))?
        .collect::<Result<BTreeSet<String>, _>>()
        .map_err(SqliteError::query("export columns"))?;
    Ok(columns
        .iter()
        .copied()
        .filter(|column| known.contains(*column))
        .collect())
}

fn ident(name: &str) -> Result<(), SqliteError> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        Ok(())
    } else {
        Err(SqliteError::Invalid {
            message: format!("invalid export identifier {name}"),
        })
    }
}

fn ident_list(order: &str) -> Result<(), SqliteError> {
    for piece in order.split([',', ' ']) {
        if piece.is_empty() || piece == "DESC" || piece == "ASC" {
            continue;
        }
        ident(piece)?;
    }
    Ok(())
}

fn cancelled(cancellation: &CancellationToken) -> Result<(), SqliteError> {
    if cancellation.is_cancelled() {
        Err(SqliteError::ExportCancelled)
    } else {
        Ok(())
    }
}

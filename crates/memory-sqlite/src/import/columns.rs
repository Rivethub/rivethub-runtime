use std::collections::BTreeSet;

use kernel::memory::SummaryKind;
use rusqlite::types::Value;
use rusqlite::{Connection, Transaction, params_from_iter};

use super::{ImportError, ImportTable, copy, values};

#[derive(Debug)]
pub(super) struct Block {
    pub(super) table: ImportTable,
    columns: Vec<Option<Column>>,
    sql: String,
}

#[derive(Debug)]
struct Column {
    name: String,
    conversion: values::Conversion,
    required: bool,
}

impl Block {
    pub(super) fn parse(
        conn: &Connection,
        name: &str,
        fields: &str,
        line: usize,
    ) -> Result<Self, ImportError> {
        let table: ImportTable = name
            .strip_prefix("ros_")
            .ok_or_else(|| copy::invalid(line, "expected a compatibility ros_* memory table"))?
            .parse()
            .map_err(|_| copy::invalid(line, format!("unsupported memory table {name}")))?;
        let mut statement = conn
            .prepare("SELECT name, type, \"notnull\", pk, dflt_value FROM pragma_table_info(?1)")?;
        let schema = statement
            .query_map([table.compatibility_name()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)? || row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut columns = Vec::new();
        let mut supplied = BTreeSet::new();
        for field in fields.split(", ") {
            let name = identifier(field, line)?.to_owned();
            if !supplied.insert(name.clone()) {
                return Err(copy::invalid(line, "duplicate COPY column"));
            }
            if table == ImportTable::Messages && name == "content_hash"
                || table == ImportTable::Summaries && name == "owner_user_id"
            {
                columns.push(None);
                continue;
            }
            let (_, sql_type, required, _) = schema
                .iter()
                .find(|(column, _, _, _)| column == &name)
                .ok_or_else(|| copy::invalid(line, format!("unknown column {name} in {table}")))?;
            columns.push(Some(Column {
                conversion: values::Conversion::for_column(&name, sql_type, line)?,
                name,
                required: *required,
            }));
        }
        for (name, _, required, default) in &schema {
            if *required && default.is_none() && !supplied.contains(name) {
                return Err(copy::invalid(
                    line,
                    format!("missing required column {name} in {table}"),
                ));
            }
        }
        let names: Vec<_> = columns
            .iter()
            .filter_map(Option::as_ref)
            .map(|column| format!("\"{}\"", column.name))
            .collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table.compatibility_name(),
            names.join(","),
            vec!["?"; names.len()].join(",")
        );
        Ok(Self {
            table,
            columns,
            sql,
        })
    }

    pub(super) fn insert(
        &self,
        tx: &Transaction<'_>,
        text: &str,
        line: usize,
        width: &mut Option<usize>,
    ) -> Result<(), ImportError> {
        let fields: Vec<_> = text.split('\t').collect();
        if fields.len() != self.columns.len() {
            return Err(copy::invalid(
                line,
                "COPY field count does not match its columns",
            ));
        }
        let mut values = Vec::new();
        for (column, text) in self.columns.iter().zip(fields) {
            let field = copy::field(text, line)?;
            let Some(column) = column else {
                continue;
            };
            let value = match field {
                None if column.required && !summary_depth(self.table, column) => {
                    return Err(copy::invalid(
                        line,
                        format!("NULL in required column {}", column.name),
                    ));
                }
                None => Value::Null,
                Some(text) => column.conversion.convert(text, line, width)?,
            };
            values.push(value);
        }
        if self.table == ImportTable::Summaries {
            rewrite_legacy_summary_kind(&self.columns, &mut values, line)?;
        }
        tx.execute(&self.sql, params_from_iter(values))?;
        Ok(())
    }
}

fn rewrite_legacy_summary_kind(
    columns: &[Option<Column>],
    values: &mut [Value],
    line: usize,
) -> Result<(), ImportError> {
    let stored: Vec<&str> = columns
        .iter()
        .filter_map(|column| column.as_ref().map(|column| column.name.as_str()))
        .collect();
    if stored.len() != values.len() {
        return Err(copy::invalid(
            line,
            "summary column count does not match values",
        ));
    }
    if let Some(kind_at) = stored.iter().position(|name| *name == "kind") {
        let kind = match values.get(kind_at) {
            Some(Value::Text(kind)) => kind.clone(),
            Some(Value::Null | Value::Integer(_) | Value::Real(_) | Value::Blob(_)) => {
                return Err(copy::invalid(line, "summary kind is not text"));
            }
            None => {
                return Err(copy::invalid(
                    line,
                    "summary column count does not match values",
                ));
            }
        };
        let depth = stored_depth(&stored, values, line)?;
        if let Some(mapped) = legacy_kind(&kind, depth, line)? {
            let slot = values
                .get_mut(kind_at)
                .ok_or_else(|| copy::invalid(line, "summary column count does not match values"))?;
            *slot = Value::Text(mapped.as_str().to_owned());
        }
    }
    if let Some(index) = stored.iter().position(|name| *name == "depth")
        && matches!(values.get(index), Some(Value::Null))
    {
        return Err(copy::invalid(line, "NULL in required column depth"));
    }
    Ok(())
}

fn summary_depth(table: ImportTable, column: &Column) -> bool {
    table == ImportTable::Summaries && column.name == "depth"
}

fn stored_depth(
    stored: &[&str],
    values: &[Value],
    line: usize,
) -> Result<Option<i64>, ImportError> {
    let Some(index) = stored.iter().position(|name| *name == "depth") else {
        return Ok(None);
    };
    match values.get(index) {
        Some(Value::Integer(depth)) => Ok(Some(*depth)),
        Some(Value::Null) => Ok(None),
        Some(Value::Real(_) | Value::Text(_) | Value::Blob(_)) => {
            Err(copy::invalid(line, "summary depth is not an integer"))
        }
        None => Err(copy::invalid(
            line,
            "summary column count does not match values",
        )),
    }
}

fn legacy_kind(
    kind: &str,
    depth: Option<i64>,
    line: usize,
) -> Result<Option<SummaryKind>, ImportError> {
    if kind.parse::<SummaryKind>().is_ok() {
        return Ok(None);
    }
    let Some(depth) = depth else {
        return Err(copy::invalid(line, "legacy summary kind needs its depth"));
    };
    match SummaryKind::from_depth(depth) {
        Some(kind) => Ok(Some(kind)),
        None => Err(copy::invalid(
            line,
            "legacy summary kind has no depth mapping",
        )),
    }
}

fn identifier(text: &str, line: usize) -> Result<&str, ImportError> {
    let text = match text.strip_prefix('"') {
        Some(quoted) => quoted
            .strip_suffix('"')
            .ok_or_else(|| copy::invalid(line, "unclosed identifier"))?,
        None => text,
    };
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(copy::invalid(line, "unsupported identifier"));
    }
    Ok(text)
}

pub(super) fn header(text: &str, line: usize) -> Result<(&str, &str), ImportError> {
    let body = text
        .strip_prefix("COPY ")
        .and_then(|body| body.strip_suffix(") FROM stdin;"))
        .ok_or_else(|| copy::invalid(line, "expected COPY table (columns) FROM stdin;"))?;
    let (table, fields) = body
        .split_once(" (")
        .ok_or_else(|| copy::invalid(line, "missing COPY column list"))?;
    let table = table
        .strip_prefix("public.")
        .or_else(|| table.strip_prefix("\"public\"."))
        .ok_or_else(|| copy::invalid(line, "COPY must name the public schema"))?;
    Ok((identifier(table, line)?, fields))
}

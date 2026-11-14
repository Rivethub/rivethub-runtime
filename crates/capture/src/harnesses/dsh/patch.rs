use std::fs;
use std::io;
use std::path::Path;

use kernel::harness_setup::SetupError;
use serde_yaml_ng::Value;

use super::super::entries;
use super::super::recall::ENTRY_NAME;
use super::BEGIN;
use super::CAPTURE_ID;
use super::EMPTY;
use super::END;
use super::FLOW;
use super::LEGACY_ENTRY;
use super::NO_NEWLINE;
use super::NOT_ARRAY;
use super::PLUGIN_NAME;
use super::RECALL_ID;
use super::UNBALANCED;
use super::config;

#[derive(Debug)]
pub(super) struct Row {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) recall: Option<RecallFields>,
}

#[derive(Debug)]
pub(super) struct RecallFields {
    pub(super) server_name: Option<String>,
    pub(super) command: String,
    pub(super) args: Vec<String>,
}

#[derive(Debug)]
pub(super) struct Region<'a> {
    prefix: &'a str,
    inner: &'a str,
    suffix: &'a str,
    empty_array: bool,
}

pub(super) fn rows_after_removal(
    patch: &Path,
    region: &Region<'_>,
    want_capture: bool,
    want_recall: bool,
) -> Result<Vec<Row>, SetupError> {
    let mut rows = owned_rows(patch, Some(region))?;
    if want_capture {
        rows.retain(|row| row.id != CAPTURE_ID && !names_our_plugin(&row.name));
    }
    if want_recall {
        rows.retain(|row| row.id != RECALL_ID && !row_runs_rh(row));
    }
    Ok(rows)
}

pub(super) fn splice_rows(
    patch: &Path,
    text: &str,
    region: Option<&Region<'_>>,
    rows: &[Row],
) -> Result<String, SetupError> {
    let block = render_block(rows);
    let after = if let Some(region) = region {
        compose_kept(region, &block)
    } else if text.trim().is_empty() {
        block
    } else {
        extend_patch(patch, text, &block)?
    };
    validate_array(patch, &after)?;
    Ok(after)
}

fn extend_patch(patch: &Path, text: &str, block: &str) -> Result<String, SetupError> {
    if !text.ends_with('\n') {
        return Err(config(patch, NO_NEWLINE));
    }
    if is_plain_empty_array(text) {
        return Ok(replace_empty_array(text, &format!("{EMPTY}{block}")));
    }
    if first_code(text).is_some_and(|line| line.starts_with('[')) {
        return Err(config(patch, FLOW));
    }
    serde_yaml_ng::from_str::<Vec<Value>>(text).map_err(|error| config(patch, not_array(error)))?;
    Ok(format!("{text}{block}"))
}

pub(super) fn compose(region: &Region<'_>, rows: &[Row]) -> Option<String> {
    if rows.is_empty() {
        return empty_remainder(region);
    }
    Some(compose_kept(region, &render_block(rows)))
}

fn compose_kept(region: &Region<'_>, block: &str) -> String {
    let mut text = region.prefix.to_owned();
    if region.empty_array {
        text.push_str(EMPTY);
    }
    text.push_str(block);
    text.push_str(region.suffix);
    text
}

fn empty_remainder(region: &Region<'_>) -> Option<String> {
    if region.empty_array {
        return Some(format!("{}[]\n{}", region.prefix, region.suffix));
    }
    let text = format!("{}{}", region.prefix, region.suffix);
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(super) fn region_of<'a>(patch: &Path, text: &'a str) -> Result<Option<Region<'a>>, SetupError> {
    let begins = text.matches(BEGIN).count();
    let ends = text.matches(END).count();
    if begins == 0 && ends == 0 {
        return Ok(None);
    }
    if begins != 1 || ends != 1 {
        return Err(config(patch, UNBALANCED));
    }
    let Some(start) = text.find(BEGIN) else {
        return Err(config(patch, UNBALANCED));
    };
    let after_begin = start.saturating_add(BEGIN.len());
    let Some(end_at) = text.get(after_begin..).and_then(|rest| rest.find(END)) else {
        return Err(config(patch, UNBALANCED));
    };
    let inner_end = after_begin.saturating_add(end_at);
    let suffix_at = inner_end.saturating_add(END.len());
    let Some(inner) = text.get(after_begin..inner_end) else {
        return Err(config(patch, UNBALANCED));
    };
    let Some(suffix) = text.get(suffix_at..) else {
        return Err(config(patch, UNBALANCED));
    };
    let empty_at = start.checked_sub(EMPTY.len());
    let empty_array = empty_at
        .and_then(|at| text.get(at..start))
        .is_some_and(|prefix| prefix == EMPTY);
    let prefix_at = if empty_array {
        empty_at.unwrap_or(start)
    } else {
        start
    };
    let Some(prefix) = text.get(..prefix_at) else {
        return Err(config(patch, UNBALANCED));
    };
    Ok(Some(Region {
        prefix,
        inner,
        suffix,
        empty_array,
    }))
}

pub(super) fn owned_rows(
    patch: &Path,
    region: Option<&Region<'_>>,
) -> Result<Vec<Row>, SetupError> {
    let Some(region) = region else {
        return Ok(Vec::new());
    };
    if region.inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let docs: Vec<Value> = serde_yaml_ng::from_str(region.inner).map_err(|error| {
        config(
            patch,
            format!("cordis.patch.yml markers do not hold a YAML array: {error}"),
        )
    })?;
    Ok(rows_of(&docs))
}

pub(super) fn capture_row() -> Row {
    Row {
        id: CAPTURE_ID.to_owned(),
        name: PLUGIN_NAME.to_owned(),
        recall: None,
    }
}

pub(super) fn recall_row(client: &Path, rh: &Path) -> Row {
    Row {
        id: RECALL_ID.to_owned(),
        name: client.display().to_string(),
        recall: Some(RecallFields {
            server_name: Some(ENTRY_NAME.to_owned()),
            command: rh.display().to_string(),
            args: vec!["mcp".to_owned()],
        }),
    }
}

fn render_block(rows: &[Row]) -> String {
    let mut text = format!("{BEGIN}- insert:\n");
    for row in rows {
        text.push_str(&render_row(row));
    }
    text.push_str(END);
    text
}

fn render_row(row: &Row) -> String {
    let head = format!(
        "    - id: {}\n      name: {}\n",
        plain_or_quoted(&row.id),
        yaml_single(&row.name)
    );
    let Some(fields) = &row.recall else {
        return head;
    };
    let mut args = String::new();
    for arg in &fields.args {
        args.push_str("          - ");
        args.push_str(&plain_or_quoted(arg));
        args.push('\n');
    }
    format!(
        "{head}      config:\n        serverName: {}\n        transport: stdio\n        command: {}\n        args:\n{args}        failOnStartupError: false\n",
        plain_or_quoted(fields.server_name.as_deref().unwrap_or(ENTRY_NAME)),
        yaml_single(&fields.command)
    )
}

pub(super) fn rows_of(docs: &[Value]) -> Vec<Row> {
    let mut rows = Vec::new();
    for doc in docs {
        let Some(insert) = doc.get("insert").and_then(Value::as_sequence) else {
            continue;
        };
        for item in insert {
            if let Some(row) = row_from(item) {
                rows.push(row);
            }
        }
    }
    rows
}

fn row_from(item: &Value) -> Option<Row> {
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    let name = item.get("name").and_then(Value::as_str)?.to_owned();
    let recall = item
        .get("config")
        .filter(|config| config.is_mapping())
        .map(|config| RecallFields {
            server_name: config
                .get("serverName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            command: command_text(config.get("command")),
            args: arg_list(config.get("args")),
        });
    Some(Row { id, name, recall })
}

pub(super) fn row_runs_rh(row: &Row) -> bool {
    let Some(fields) = &row.recall else {
        return false;
    };
    let mut text = fields.command.clone();
    for arg in &fields.args {
        text.push(' ');
        text.push_str(arg);
    }
    entries::runs_rh_mcp(&text)
}

pub(super) fn names_our_recall(row: &Row) -> bool {
    row.id == RECALL_ID
        || row
            .recall
            .as_ref()
            .is_some_and(|fields| fields.server_name.as_deref().is_some_and(is_memory_name))
}

pub(super) fn row_server(row: &Row) -> String {
    row.recall
        .as_ref()
        .and_then(|fields| fields.server_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| row.id.clone())
}

pub(super) fn names_our_plugin(name: &str) -> bool {
    name == PLUGIN_NAME || name.ends_with("/plugins/rivethub-capture.mjs")
}

fn is_memory_name(name: &str) -> bool {
    name == ENTRY_NAME || name == LEGACY_ENTRY
}

pub(super) fn read_patch(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn validate_array(path: &Path, text: &str) -> Result<(), SetupError> {
    serde_yaml_ng::from_str::<Vec<Value>>(text)
        .map(|_| ())
        .map_err(|error| {
            config(
                path,
                format!("cordis.patch.yml would not be a YAML array: {error}"),
            )
        })
}

fn is_plain_empty_array(text: &str) -> bool {
    let mut arrays: usize = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[]" {
            arrays = arrays.saturating_add(1);
            continue;
        }
        return false;
    }
    arrays == 1 && serde_yaml_ng::from_str::<Vec<Value>>(text).is_ok_and(|items| items.is_empty())
}

fn replace_empty_array(text: &str, block: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        if !replaced && line.trim() == "[]" {
            out.push_str(block);
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

fn first_code(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

fn command_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        Some(_) | None => String::new(),
    }
}

fn arg_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(text)) => vec![text.clone()],
        Some(_) | None => Vec::new(),
    }
}

fn plain_or_quoted(text: &str) -> String {
    if is_plain(text) {
        text.to_owned()
    } else {
        yaml_single(text)
    }
}

fn is_plain(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && text != "true"
        && text != "false"
        && text != "null"
}

fn yaml_single(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

pub(super) fn not_array(error: impl std::fmt::Display) -> String {
    format!("{NOT_ARRAY}: {error}")
}

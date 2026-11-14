use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::StoreSession;
use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use wire::harness::HarnessId;

use super::field::Field;
use super::field::object_line;
use super::field::text_of;
use super::files::directories;
use super::files::entries;
use super::files::file_times;
use super::files::head;
use super::files::open_store;
use super::files::whole_file;
use super::text::TextElement;
use super::text::clip;
use super::text::collapse;
use super::text::non_empty;
use super::time::FileTimes;
use super::title::HarnessTitles;
use crate::readers::codex::codex_id;
use crate::store_path::Root;

const HEAD_WINDOW: u64 = 65_536;
const TITLE_CHARS: usize = 120;

#[derive(Debug, Clone)]
struct Rollout {
    id: String,
    path: PathBuf,
    times: FileTimes,
}

fn digits(name: &str, width: usize) -> bool {
    name.len() == width && name.bytes().all(|byte| byte.is_ascii_digit())
}

fn rollouts(store: &Root) -> Vec<Rollout> {
    let mut found = Vec::new();
    let dated = |dir: &Path, width: usize| {
        directories(store, dir)
            .into_iter()
            .filter(move |entry| digits(&entry.name, width))
    };
    for year in dated(store.canonical(), 4) {
        for month in dated(&year.path, 2) {
            for day in dated(&month.path, 2) {
                for entry in entries(store, &day.path) {
                    let id = entry.name.strip_suffix(".jsonl").and_then(codex_id);
                    if let Some(id) = id.filter(|_| entry.is_file())
                        && let Some(times) = file_times(&entry.path)
                    {
                        found.push(Rollout {
                            id,
                            path: entry.path,
                            times,
                        });
                    }
                }
            }
        }
    }
    found
}

fn sessions(root: &Path) -> Result<Option<Root>, TranscriptReaderError> {
    open_store(&root.join("sessions"))
}

pub(crate) fn list(root: &Path, limit: usize) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    let Some(store) = sessions(root)? else {
        return Ok(Vec::new());
    };
    let mut found = rollouts(&store);
    found.sort_by_key(|item| std::cmp::Reverse(item.times.mtime));
    let names = thread_names(root);
    Ok(found
        .iter()
        .filter(|rollout| names.contains_key(&rollout.id) || !unprompted(&rollout.path))
        .take(limit)
        .map(|rollout| rollout_row(rollout, &names))
        .collect())
}

pub(crate) fn describe(
    root: &Path,
    id: &str,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    Ok(describe_many(root, &[id.to_owned()])?.pop())
}

pub(crate) fn describe_many(
    root: &Path,
    ids: &[String],
) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    let Some(store) = sessions(root)? else {
        return Ok(Vec::new());
    };
    let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let mut newest: BTreeMap<String, Rollout> = BTreeMap::new();
    for rollout in rollouts(&store)
        .into_iter()
        .filter(|rollout| wanted.contains(rollout.id.as_str()))
    {
        match newest.get(&rollout.id) {
            Some(current) if current.times.mtime >= rollout.times.mtime => {}
            Some(_) | None => {
                newest.insert(rollout.id.clone(), rollout);
            }
        }
    }
    let names = thread_names(root);
    Ok(newest
        .values()
        .map(|rollout| rollout_row(rollout, &names))
        .collect())
}

#[derive(Debug, Deserialize)]
struct IndexLine {
    id: String,
    thread_name: String,
}

fn thread_names(root: &Path) -> HashMap<String, String> {
    let index = root.join("session_index.jsonl");
    if !index.is_file() {
        return HashMap::new();
    }
    let Some(text) = whole_file(&index) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(object_line::<IndexLine>)
        .map(|line| (line.id, line.thread_name))
        .collect()
}

fn rollout_row(rollout: &Rollout, names: &HashMap<String, String>) -> StoreSession {
    let mut row = StoreSession::new(HarnessId::Codex, &rollout.id, rollout.times.mtime);
    row.created_at = Some(rollout.times.birth);
    if let Some(title) = non_empty(
        collapse(&codex_title(&rollout.path))
            .chars()
            .take(TITLE_CHARS)
            .collect(),
    ) {
        row.title = title;
    }
    if let Some(name) = names.get(&rollout.id) {
        let mut titles = HarnessTitles::default();
        titles.manual(name);
        titles.apply(&mut row);
    }
    row
}

#[derive(Debug, Deserialize)]
struct RolloutLine {
    #[serde(rename = "type")]
    kind: Option<Field>,
    payload: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct RolloutPayload {
    #[serde(rename = "type")]
    kind: Option<Field>,
    role: Option<Field>,
    content: Option<Field>,
}

fn user_message(line: &str) -> Option<String> {
    let parsed = object_line::<RolloutLine>(line)?;
    if !parsed.kind?.is_text("response_item") {
        return None;
    }
    let payload = parsed.payload?.object::<RolloutPayload>()?;
    if !payload.kind?.is_text("message") || !payload.role?.is_text("user") {
        return None;
    }
    let content = payload.content?;
    let text = match content.text() {
        Some(text) => text,
        None => content
            .items()?
            .iter()
            .filter_map(Field::object::<TextElement>)
            .filter(|element| {
                element
                    .kind
                    .as_ref()
                    .is_some_and(|kind| kind.is_text("input_text") || kind.is_text("text"))
            })
            .filter_map(|element| text_of(element.text.as_ref()))
            .collect(),
    };
    let trimmed = text.trim();
    if injected(trimmed) {
        return None;
    }
    kernel::memory::spoken_title(trimmed).map(|spoken| clip(spoken, TITLE_CHARS))
}

pub(crate) fn injected(text: &str) -> bool {
    [
        "<environment_context>",
        "<skills_instructions>",
        "<multi_agent_",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

fn unprompted(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.len() <= HEAD_WINDOW)
        && codex_title(path).is_empty()
}

pub(crate) fn codex_title(path: &Path) -> String {
    head(path, HEAD_WINDOW)
        .and_then(|text| text.lines().find_map(user_message))
        .unwrap_or_default()
}

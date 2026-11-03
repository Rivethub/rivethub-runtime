use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use kernel::transcript::SessionRenameError;
use kernel::transcript::StoreSession;
use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use serde::Serialize;
use wire::harness::HarnessId;

use super::ancestors::with_ancestors;
use super::field::Field;
use super::field::first_present;
use super::field::object_line;
use super::field::text_of;
use super::files::Entry;
use super::files::append_line;
use super::files::directories;
use super::files::entries;
use super::files::file_times;
use super::files::first_cwd;
use super::files::head;
use super::files::newest_per_key;
use super::files::open_store;
use super::files::regular_file;
use super::files::tail;
use super::files::whole_file;
use super::files::write_failure;
use super::text::clip;
use super::text::clip_field;
use super::text::is_uuid;
use super::text::non_empty;
use super::text::segment_safe;
use super::text::strip_paste;
use super::time::FileTimes;
use super::title::HarnessTitles;
use crate::store_path::Root;

const HEAD_WINDOW: u64 = 65_536;
const TITLE_CHARS: usize = 120;
const AGENT_CHARS: usize = 64;
const MODEL_CHARS: usize = 80;
const NAMING_MARKER: &str = "-title\"";
const MODE_MARKER: &str = "\"permissionMode\"";
const ASSISTANT_MARKER: &str = "\"type\":\"assistant\"";
const MODE_WINDOW: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Transcript {
    id: String,
    parent: Option<String>,
    path: PathBuf,
    times: FileTimes,
}

impl Transcript {
    fn key(&self) -> (Option<String>, String) {
        (self.parent.clone(), self.id.clone())
    }

    fn row(&self) -> StoreSession {
        let mut row = StoreSession::new(HarnessId::ClaudeCode, &self.id, self.times.mtime);
        row.created_at = Some(self.times.birth);
        row.parent_session_id.clone_from(&self.parent);
        row
    }

    fn enrich(&self, row: &mut StoreSession) {
        if let Some(title) = non_empty(claude_title(&self.path)) {
            row.title = title;
        }
        if self.parent.is_some() {
            let labels = agent_labels(&self.path);
            row.agent_name = non_empty(labels.agent_name);
            row.model = non_empty(labels.model);
        } else {
            let tail = tail(&self.path, HEAD_WINDOW);
            harness_titles(&self.path, tail.as_deref()).apply(row);
            row.model = tail.as_deref().and_then(answered_model);
        }
    }
}

fn projects(root: &Path) -> Result<Option<Root>, TranscriptReaderError> {
    open_store(&root.join("projects"))
}

pub(crate) fn list(root: &Path, limit: usize) -> Result<Vec<StoreSession>, TranscriptReaderError> {
    let Some(store) = projects(root)? else {
        return Ok(Vec::new());
    };
    let mut kept = dedup(transcripts(&store));
    kept.sort_by_key(|item| std::cmp::Reverse(item.times.mtime));
    let rows = with_ancestors(kept.iter().map(Transcript::row).collect(), limit);
    let by_key: HashMap<(Option<String>, String), &Transcript> = kept
        .iter()
        .map(|transcript| (transcript.key(), transcript))
        .collect();
    Ok(rows
        .into_iter()
        .map(|mut row| {
            let own = (row.parent_session_id.clone(), row.id.clone());
            let file = by_key.get(&own).or_else(|| {
                row.parent_session_id
                    .as_ref()
                    .and_then(|_| by_key.get(&(None, row.id.clone())))
            });
            if let Some(file) = file {
                file.enrich(&mut row);
            }
            row
        })
        .collect())
}

fn slugs(store: &Root) -> Vec<Entry> {
    directories(store, store.canonical())
        .into_iter()
        .filter(|slug| segment_safe(&slug.name))
        .collect()
}

fn transcripts(store: &Root) -> Vec<Transcript> {
    let mut found = Vec::new();
    for slug in slugs(store) {
        for entry in entries(store, &slug.path) {
            if !segment_safe(&entry.name) {
                continue;
            }
            if entry.is_file() {
                if let Some(id) = entry
                    .name
                    .strip_suffix(".jsonl")
                    .filter(|id| !id.is_empty())
                    && let Some(times) = file_times(&entry.path)
                {
                    found.push(Transcript {
                        id: id.to_owned(),
                        parent: None,
                        path: entry.path.clone(),
                        times,
                    });
                }
            } else if entry.is_dir() {
                found.extend(agent_files(store, &entry.path, &entry.name));
            }
        }
    }
    found
}

fn agent_files(store: &Root, session_dir: &Path, parent: &str) -> Vec<Transcript> {
    let subagents = session_dir.join("subagents");
    let mut places = vec![subagents.clone()];
    places.extend(
        directories(store, &subagents.join("workflows"))
            .into_iter()
            .filter(|run| segment_safe(&run.name))
            .map(|run| run.path),
    );
    places.push(session_dir.to_path_buf());
    let mut found = Vec::new();
    for place in places {
        for entry in entries(store, &place) {
            let Some(id) = agent_id(&entry.name) else {
                continue;
            };
            if !segment_safe(id) || !segment_safe(parent) || id == parent || !entry.is_file() {
                continue;
            }
            if let Some(times) = file_times(&entry.path) {
                found.push(Transcript {
                    id: id.to_owned(),
                    parent: Some(parent.to_owned()),
                    path: entry.path,
                    times,
                });
            }
        }
    }
    found
}

fn agent_id(name: &str) -> Option<&str> {
    name.strip_prefix("agent-")?
        .strip_suffix(".jsonl")
        .filter(|id| !id.is_empty())
}

fn dedup(found: Vec<Transcript>) -> Vec<Transcript> {
    let mut kept = newest_per_key(found, Transcript::key, |transcript| transcript.times.mtime);
    let top_level: HashSet<String> = kept
        .iter()
        .filter(|transcript| transcript.parent.is_none())
        .map(|transcript| transcript.id.clone())
        .collect();
    kept.retain(|transcript| transcript.parent.is_none() || !top_level.contains(&transcript.id));
    kept
}

#[derive(Debug, Deserialize)]
struct TitleLine {
    #[serde(rename = "type")]
    kind: Option<Field>,
    summary: Option<Field>,
    message: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct MessageContent {
    content: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct TextPart {
    text: Option<Field>,
}

pub(crate) fn claude_title(path: &Path) -> String {
    let Some(text) = head(path, HEAD_WINDOW) else {
        return String::new();
    };
    for line in text.lines() {
        let Some(parsed) = object_line::<TitleLine>(line) else {
            continue;
        };
        let Some(kind) = text_of(parsed.kind.as_ref()) else {
            continue;
        };
        if kind == "summary" {
            if let Some(summary) =
                text_of(parsed.summary.as_ref()).filter(|summary| !summary.trim().is_empty())
            {
                return clip(&summary, TITLE_CHARS);
            }
        } else if kind == "user" {
            let typed = parsed.message.as_ref().map(user_concat).unwrap_or_default();
            if !typed.trim().is_empty() {
                return clip(&strip_paste(&typed), TITLE_CHARS);
            }
        }
    }
    String::new()
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum NamingLine {
    CustomTitle { custom_title: String },
    AiTitle { ai_title: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename = "custom-title", rename_all = "camelCase")]
struct CustomTitleLine<'a> {
    custom_title: &'a str,
    session_id: &'a str,
}

fn harness_titles(path: &Path, tail: Option<&str>) -> HarnessTitles {
    let mut titles = HarnessTitles::default();
    let head = head(path, HEAD_WINDOW);
    for line in [head.as_deref(), tail]
        .into_iter()
        .flatten()
        .flat_map(str::lines)
    {
        if !line.contains(NAMING_MARKER) {
            continue;
        }
        match object_line::<NamingLine>(line) {
            Some(NamingLine::CustomTitle { custom_title }) => titles.manual(&custom_title),
            Some(NamingLine::AiTitle { ai_title }) => titles.automatic(&ai_title),
            None => {}
        }
    }
    titles
}

fn answered_model(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .filter(|line| line.contains(ASSISTANT_MARKER))
        .filter_map(object_line::<LabelLine>)
        .map(|line| line.line_model())
        .find(|model| !model.is_empty() && !model.starts_with('<'))
}

pub(crate) fn rename(
    root: &Path,
    id: &str,
    title: &str,
) -> Result<Option<StoreSession>, SessionRenameError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let Some(store) = projects(root)? else {
        return Ok(None);
    };
    let Some(file) = top_level_file(&store, id) else {
        return Ok(None);
    };
    let line = serde_json::to_string(&CustomTitleLine {
        custom_title: title,
        session_id: id,
    })
    .map_err(|error| write_failure(&file.path, error.into()))?;
    append_line(&file.path, &line).map_err(|error| write_failure(&file.path, error))?;
    Ok(describe(root, id, None)?)
}

fn user_concat(message: &Field) -> String {
    let Some(content) = message
        .object::<MessageContent>()
        .and_then(|message| message.content)
    else {
        return String::new();
    };
    if let Some(text) = content.text() {
        return text;
    }
    content
        .items()
        .unwrap_or_default()
        .iter()
        .filter_map(Field::object::<TextPart>)
        .filter_map(|part| text_of(part.text.as_ref()))
        .collect()
}

#[derive(Debug, Deserialize)]
struct LabelLine {
    #[serde(rename = "agentType")]
    camel_agent_type: Option<Field>,
    subagent_type: Option<Field>,
    agent_type: Option<Field>,
    model: Option<Field>,
    message: Option<Field>,
}

#[derive(Debug, Deserialize)]
struct MessageModel {
    model: Option<Field>,
}

impl LabelLine {
    fn agent_name(&self) -> String {
        clip_field(
            first_present(&[
                self.camel_agent_type.as_ref(),
                self.subagent_type.as_ref(),
                self.agent_type.as_ref(),
            ]),
            AGENT_CHARS,
        )
    }

    fn line_model(&self) -> String {
        match self.message.as_ref().filter(|message| message.is_object()) {
            Some(message) => clip_field(
                message
                    .object::<MessageModel>()
                    .and_then(|message| message.model)
                    .as_ref(),
                MODEL_CHARS,
            ),
            None => clip_field(self.model.as_ref(), MODEL_CHARS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentLabels {
    pub(crate) agent_name: String,
    pub(crate) model: String,
}

pub(crate) fn agent_labels(path: &Path) -> AgentLabels {
    let mut labels = AgentLabels::default();
    let meta_path = path.with_extension("meta.json");
    if meta_path.is_file()
        && let Some(meta) = whole_file(&meta_path).and_then(|text| object_line::<LabelLine>(&text))
    {
        labels.agent_name = meta.agent_name();
        labels.model = clip_field(meta.model.as_ref(), MODEL_CHARS);
    }
    if !labels.agent_name.is_empty() && !labels.model.is_empty() {
        return labels;
    }
    let Some(text) = head(path, HEAD_WINDOW) else {
        return labels;
    };
    for line in text.lines() {
        let Some(parsed) = object_line::<LabelLine>(line) else {
            continue;
        };
        if labels.agent_name.is_empty() {
            labels.agent_name = parsed.agent_name();
        }
        if labels.model.is_empty() {
            labels.model = parsed.line_model();
        }
        if !labels.agent_name.is_empty() && !labels.model.is_empty() {
            break;
        }
    }
    labels
}

fn top_level_file(store: &Root, id: &str) -> Option<Transcript> {
    let mut newest: Option<Transcript> = None;
    for slug in slugs(store) {
        let path = slug.path.join(format!("{id}.jsonl"));
        if !regular_file(store, &path) {
            continue;
        }
        let Some(times) = file_times(&path) else {
            continue;
        };
        if newest
            .as_ref()
            .is_none_or(|current| times.mtime > current.times.mtime)
        {
            newest = Some(Transcript {
                id: id.to_owned(),
                parent: None,
                path,
                times,
            });
        }
    }
    newest
}

pub(crate) fn store_cwd(root: &Path, id: &str) -> Result<Option<String>, TranscriptReaderError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let Some(store) = projects(root)? else {
        return Ok(None);
    };
    match top_level_file(&store, id) {
        Some(file) => first_cwd(&file.path),
        None => Ok(None),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModeLine {
    permission_mode: Option<String>,
}

pub(crate) fn permission_mode(
    root: &Path,
    id: &str,
) -> Result<Option<String>, TranscriptReaderError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let Some(store) = projects(root)? else {
        return Ok(None);
    };
    Ok(top_level_file(&store, id).and_then(|file| latest_mode(&file.path)))
}

fn latest_mode(path: &Path) -> Option<String> {
    tail(path, MODE_WINDOW)?
        .lines()
        .rev()
        .filter(|line| line.contains(MODE_MARKER))
        .find_map(|line| {
            object_line::<ModeLine>(line)?
                .permission_mode
                .filter(|mode| !mode.trim().is_empty())
        })
}

pub(crate) fn describe(
    root: &Path,
    id: &str,
    parent: Option<&str>,
) -> Result<Option<StoreSession>, TranscriptReaderError> {
    if !segment_safe(id) {
        return Ok(None);
    }
    let Some(store) = projects(root)? else {
        return Ok(None);
    };
    if parent.is_none() {
        if let Some(file) = top_level_file(&store, id) {
            return Ok(Some(described(&file)));
        }
        if is_uuid(id) {
            return Ok(None);
        }
    }
    let matches = agent_matches(&store, id, parent);
    let parents: HashSet<&Option<String>> = matches
        .iter()
        .map(|transcript| &transcript.parent)
        .collect();
    if parent.is_none() && parents.len() > 1 {
        return Ok(None);
    }
    Ok(newest(&matches).map(described))
}

fn agent_matches(store: &Root, id: &str, parent: Option<&str>) -> Vec<Transcript> {
    let mut matches = Vec::new();
    for slug in slugs(store) {
        for session in directories(store, &slug.path) {
            if session.name.contains('.') || parent.is_some_and(|parent| parent != session.name) {
                continue;
            }
            matches.extend(
                agent_files(store, &session.path, &session.name)
                    .into_iter()
                    .filter(|transcript| transcript.id == id),
            );
        }
    }
    matches
}

fn newest(matches: &[Transcript]) -> Option<&Transcript> {
    matches
        .iter()
        .fold(None::<&Transcript>, |newest, candidate| match newest {
            Some(current) if current.times.mtime >= candidate.times.mtime => Some(current),
            Some(_) | None => Some(candidate),
        })
}

pub(crate) fn transcript_file(
    root: &Path,
    id: &str,
) -> Result<Option<PathBuf>, TranscriptReaderError> {
    if !segment_safe(id) {
        return Ok(None);
    }
    let Some(store) = projects(root)? else {
        return Ok(None);
    };
    if let Some(file) = top_level_file(&store, id) {
        return Ok(Some(file.path));
    }
    if is_uuid(id) {
        return Ok(None);
    }
    Ok(newest(&agent_matches(&store, id, None)).map(|file| file.path.clone()))
}

fn described(file: &Transcript) -> StoreSession {
    let mut row = file.row();
    file.enrich(&mut row);
    row
}

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::Digest as _;
use sha2::Sha256;
use tempfile::TempDir;
use testkit::Json;
use wire::JsonObject;
use wire::message::Role;

pub(crate) type TestError = Box<dyn std::error::Error>;
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

pub(crate) const RECORDED_START: &str = "2026-10-10T12:00:00.000Z";

pub(crate) fn recording<T: DeserializeOwned>(name: &str) -> TestResult<T> {
    let path = testkit::recording(&format!("memory-sqlite/{name}"));
    let text = fs::read_to_string(&path).map_err(|error| format!("read {name}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {name}: {error}").into())
}

pub(crate) fn instant(text: &str) -> TestResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)
        .map_err(|error| format!("parse instant {text}: {error}"))?
        .with_timezone(&Utc))
}

#[derive(Debug, Deserialize)]
pub(crate) struct Step {
    pub(crate) label: String,
    pub(crate) at: String,
    pub(crate) result: Option<Json>,
    pub(crate) error: Option<RecordedError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordedError {
    pub(crate) message: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Scenario {
    pub(crate) name: String,
    pub(crate) steps: Vec<Step>,
    #[serde(default)]
    pub(crate) tables: HashMap<String, Vec<Json>>,
    pub(crate) logs: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScenarioFile {
    pub(crate) scenarios: Vec<Scenario>,
}

#[derive(Debug)]
pub(crate) struct Fresh {
    _dir: TempDir,
    pub(crate) store: SqliteStore,
}

pub(crate) fn fresh_store() -> TestResult<Fresh> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite_at(&path, instant(RECORDED_START)?)?;
    let store = SqliteStore::open(&path)?;
    Ok(Fresh { _dir: dir, store })
}

pub(crate) async fn table(store: &SqliteStore, name: &str) -> TestResult<Vec<Json>> {
    let sql = format!("SELECT * FROM {name} ORDER BY rowid");
    let rows = store
        .with_conn(move |conn| Ok(testkit::query_json(conn, &sql, &[])))
        .await??;
    Ok(rows)
}

pub(crate) fn seed(source: rusqlite::Error) -> SqliteError {
    SqliteError::Query {
        operation: "seed",
        source,
    }
}

pub(crate) async fn execute(store: &SqliteStore, sql: &str) -> TestResult {
    let sql = sql.to_owned();
    store
        .with_conn(move |conn| Ok(conn.execute_batch(&sql)))
        .await??;
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct Ids {
    recorded_to_actual: HashMap<String, String>,
    actual_to_recorded: HashMap<String, String>,
}

pub(crate) fn generated(text: &str) -> bool {
    text.strip_prefix("00000000-0000-4000-8000-0000")
        .is_some_and(|tail| tail.len() == 8 && tail.bytes().all(|byte| byte.is_ascii_digit()))
}

fn structured(text: &str) -> Option<Json> {
    serde_json::from_str::<Json>(text)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
}

fn timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

impl Ids {
    pub(crate) fn actual(&self, recorded: &str) -> TestResult<String> {
        self.recorded_to_actual
            .get(recorded)
            .cloned()
            .ok_or_else(|| format!("recorded id {recorded} has not been matched yet").into())
    }

    pub(crate) fn recorded(&self, actual: &str) -> TestResult<String> {
        self.actual_to_recorded
            .get(actual)
            .cloned()
            .ok_or_else(|| format!("actual id {actual} has no recorded role").into())
    }

    pub(crate) fn bind(&mut self, recorded: &str, actual: &str) -> Result<(), String> {
        match (
            self.recorded_to_actual.get(recorded),
            self.actual_to_recorded.get(actual),
        ) {
            (None, None) => {
                self.recorded_to_actual
                    .insert(recorded.to_owned(), actual.to_owned());
                self.actual_to_recorded
                    .insert(actual.to_owned(), recorded.to_owned());
                Ok(())
            }
            (Some(bound), _) if bound == actual => Ok(()),
            (bound, back) => Err(format!(
                "id role mismatch: recorded {recorded} (bound to {bound:?}) vs actual {actual} (bound to {back:?})"
            )),
        }
    }

    pub(crate) fn same(&mut self, recorded: &Json, actual: &Json, at: &str) -> Result<(), String> {
        match (recorded, actual) {
            (Json::Object(rec), Json::String(text)) if rec.contains_key("sha256") => {
                long_text(recorded, text, at)
            }
            (Json::Object(rec), Json::Object(act)) => {
                let keys: BTreeSet<&String> = rec.keys().chain(act.keys()).collect();
                keys.into_iter().try_for_each(|key| {
                    self.same(
                        rec.get(key).unwrap_or(&Json::Null),
                        act.get(key).unwrap_or(&Json::Null),
                        &format!("{at}.{key}"),
                    )
                })
            }
            (Json::Array(rec), Json::Array(act)) if rec.len() == act.len() => rec
                .iter()
                .zip(act)
                .enumerate()
                .try_for_each(|(index, (r, a))| self.same(r, a, &format!("{at}[{index}]"))),
            (Json::String(rec), Json::String(act)) => self.same_text(rec, act, at),
            (Json::String(rec), Json::Number(millis))
                if timestamp(rec).map(|instant| instant.timestamp_millis()) == millis.as_i64() =>
            {
                Ok(())
            }
            (Json::Number(rec), Json::Number(act)) if rec.as_f64() == act.as_f64() => Ok(()),
            (rec, act) if rec == act => Ok(()),
            (rec, act) => Err(format!("{at}: recorded {rec} vs actual {act}")),
        }
    }

    fn same_text(&mut self, rec: &str, act: &str, at: &str) -> Result<(), String> {
        if rec == act {
            return Ok(());
        }
        if let (Some(r), Some(a)) = (structured(rec), structured(act)) {
            return self.same(&r, &a, at);
        }
        if let (Some(r), Some(a)) = (timestamp(rec), timestamp(act))
            && r == a
        {
            return Ok(());
        }
        self.same_slots(rec, act, at)
    }

    fn same_slots(&mut self, rec: &str, act: &str, at: &str) -> Result<(), String> {
        let recorded = split_ids(rec);
        let actual = split_ids(act);
        if recorded.len() != actual.len() {
            return Err(format!("{at}: recorded {rec:?} vs actual {act:?}"));
        }
        for (left, right) in recorded.into_iter().zip(actual) {
            match (left, right) {
                (Slot::Literal(recorded_text), Slot::Literal(actual_text))
                    if recorded_text == actual_text => {}
                (Slot::Uuid(recorded_id), Slot::Uuid(actual_id)) if generated(&recorded_id) => {
                    self.bind(&recorded_id, &actual_id)
                        .map_err(|error| format!("{at}: {error}"))?;
                }
                (Slot::Uuid(recorded_id), Slot::Uuid(actual_id)) if recorded_id == actual_id => {}
                (Slot::Literal(_) | Slot::Uuid(_), Slot::Literal(_) | Slot::Uuid(_)) => {
                    return Err(format!("{at}: recorded {rec:?} vs actual {act:?}"));
                }
            }
        }
        Ok(())
    }
}

enum Slot {
    Literal(String),
    Uuid(String),
}

fn split_ids(text: &str) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut rest = text;
    let mut literal = String::new();
    while !rest.is_empty() {
        if let Some(id) = take_uuid(rest) {
            push_literal(&mut slots, &mut literal);
            slots.push(Slot::Uuid(id.to_owned()));
            match rest.get(id.len()..) {
                Some(tail) => rest = tail,
                None => break,
            }
        } else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            literal.push(ch);
            match rest.get(ch.len_utf8()..) {
                Some(tail) => rest = tail,
                None => break,
            }
        }
    }
    push_literal(&mut slots, &mut literal);
    slots
}

fn push_literal(slots: &mut Vec<Slot>, literal: &mut String) {
    if !literal.is_empty() {
        slots.push(Slot::Literal(std::mem::take(literal)));
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct LongText {
    length: usize,
    sha256: String,
    head: String,
    tail: String,
}

fn long_text(recorded: &Json, text: &str, at: &str) -> Result<(), String> {
    let expected: LongText = serde_json::from_value(recorded.clone())
        .map_err(|error| format!("{at}: long text cell: {error}"))?;
    let chars: Vec<char> = text.chars().collect();
    let actual = LongText {
        length: chars.len(),
        sha256: hex::encode(Sha256::digest(text.as_bytes())),
        head: chars.iter().take(8).collect(),
        tail: chars.iter().skip(chars.len().saturating_sub(8)).collect(),
    };
    if expected == actual {
        Ok(())
    } else {
        Err(format!("{at}: recorded {expected:?} vs actual {actual:?}"))
    }
}

pub(crate) fn object(text: &str) -> TestResult<JsonObject> {
    Ok(serde_json::from_str(text)?)
}

pub(crate) fn message(event_id: &str, role: Role, content: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

pub(crate) fn batch(session_key: &str, agent: &str, channel: &str) -> CaptureBatchRequest {
    CaptureBatchRequest {
        session_key: session_key.to_owned(),
        agent: agent.to_owned(),
        channel: Some(channel.to_owned()),
        title: None,
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: Vec::new(),
    }
}

pub(crate) fn deploy_messages() -> TestResult<Vec<CaptureMessage>> {
    let mut assistant = message(
        "e2",
        Role::Assistant,
        "run the deploy script from the release branch, then watch the health check",
    );
    assistant.created_at = Some(instant("2026-10-04T08:00:00.000Z")?);
    let mut tool = message("e3", Role::Tool, "");
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(object(r#"{"command":"make deploy"}"#)?);
    tool.tool_result = Some("deployed build 42 to staging.example.com".to_owned());
    Ok(vec![
        message(
            "e1",
            Role::User,
            "how do we deploy the AcmeKit service to staging",
        ),
        assistant,
        tool,
    ])
}

pub(crate) fn deploy_batch(session_key: &str, agent: &str) -> TestResult<CaptureBatchRequest> {
    let mut deploy = batch(session_key, agent, "cli");
    deploy.messages = deploy_messages()?;
    Ok(deploy)
}

pub(crate) async fn compare_tables(
    store: &SqliteStore,
    recorded: &HashMap<String, Vec<Json>>,
    ids: &mut Ids,
    scenario: &str,
) -> TestResult {
    let mut names: Vec<&String> = recorded.keys().collect();
    names.sort_by_key(|name| (name.as_str() != "ros_conversations", name.as_str()));
    for name in names {
        let actual = table(store, name).await?;
        let expected = Json::Array(recorded[name].clone());
        ids.same(
            &expected,
            &Json::Array(actual.clone()),
            &format!("{scenario}: {name}"),
        )
        .map_err(|error| format!("{error}\nactual rows: {}", Json::Array(actual)))?;
    }
    Ok(())
}

pub(crate) fn expect_json(recorded: &Json, actual: &Json, at: &str, ids: &mut Ids) -> TestResult {
    ids.same(recorded, actual, at)
        .map_err(|error| format!("{error}\nactual: {actual}"))?;
    Ok(())
}

pub(crate) fn expect_logs(
    recorded: &[String],
    actual: &[String],
    at: &str,
    ids: &mut Ids,
) -> TestResult {
    let recorded = Json::Array(recorded.iter().cloned().map(Json::String).collect());
    let actual_json = Json::Array(actual.iter().cloned().map(Json::String).collect());
    expect_json(&recorded, &actual_json, at, ids)
}

fn take_uuid(text: &str) -> Option<&str> {
    let head = text.as_bytes().get(..36)?;
    if uuid_bytes(head) {
        text.get(..36)
    } else {
        None
    }
}

fn uuid_bytes(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    })
}

pub(crate) fn owned_by_the_caller(rows: &mut [Json]) {
    for row in rows {
        if let Some(row) = row.as_object_mut()
            && row.get("owner_user_id") == Some(&Json::Null)
        {
            row.insert("owner_user_id".to_owned(), Json::from(testkit::OWNER));
        }
    }
}

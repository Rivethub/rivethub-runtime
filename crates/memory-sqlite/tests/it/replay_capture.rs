use crate::common;

use common::Ids;
use common::Scenario;
use common::ScenarioFile;
use common::TestResult;
use common::batch;
use common::compare_tables;
use common::deploy_batch;
use common::fresh_store;
use common::instant;
use common::message;
use common::object;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::MemoryEntry;
use kernel::owner::Owner;
use testkit::Json;
use wire::message::Role;

const SKIPPED_SCENARIOS: [(&str, &str); 2] = [
    (
        "project rule tag at capture",
        "the project rule resolves cwd through the filesystem and writes tags; project tagging is outside this slice",
    ),
    (
        "captured messages queued for embedding, then drained",
        "embedding job enqueue and drain are outside this slice",
    ),
];

const HISTORY_SKIP: &str = "session and task history reads are not part of this slice";
const SETTINGS_SKIP: &str = "session settings and task association are not part of this slice";
const BAD_STAMP: &str = "a capture timestamp is DateTime<Utc>; a non-instant cannot be constructed, and the recorded rejection is a parse throw outside the store";

#[derive(Debug)]
enum Action {
    Capture(CaptureBatchRequest),
    Append(MemoryEntry),
    Skip(&'static str),
}

#[derive(Debug)]
struct Deviation {
    table: &'static str,
    row: usize,
    column: &'static str,
    value: Json,
    reason: &'static str,
}

#[derive(Debug, Default)]
struct Plan {
    owner: Option<&'static str>,
    steps: Vec<(&'static str, Action)>,
    deviations: Vec<Deviation>,
}

fn entry(session: &str, channel: &str, role: Role, content: &str) -> MemoryEntry {
    MemoryEntry {
        provenance: None,
        id: None,
        session_id: session.to_owned(),
        agent: "rivet".to_owned(),
        channel: channel.to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

fn resent_batch() -> TestResult<Plan> {
    let mut again = deploy_batch("cli:sess-1", "rivet")?;
    again.messages.push(message(
        "e4",
        Role::User,
        "and how do we roll it back afterwards",
    ));
    Ok(Plan {
        steps: vec![
            (
                "capture batch",
                Action::Capture(deploy_batch("cli:sess-1", "rivet")?),
            ),
            ("capture batch + e4", Action::Capture(again)),
            ("getSessionHistory", Action::Skip(HISTORY_SKIP)),
        ],
        ..Plan::default()
    })
}

fn same_event_elsewhere() -> TestResult<Plan> {
    Ok(Plan {
        steps: vec![
            (
                "capture",
                Action::Capture(deploy_batch("cli:sess-1", "rivet")?),
            ),
            (
                "capture other session",
                Action::Capture(deploy_batch("cli:sess-2", "rivet")?),
            ),
            (
                "capture other agent",
                Action::Capture(deploy_batch("cli:sess-1", "helper")?),
            ),
        ],
        ..Plan::default()
    })
}

fn kept_unless_supplied() -> TestResult<Plan> {
    let renamed = "Deploy notes, renamed";
    let mut first = deploy_batch("cli:sess-1", "rivet")?;
    first.title = Some("Deploy notes".to_owned());
    first.settings = Some(object(r#"{"cwd":"/home/user/work/acme-app","depth":2.5}"#)?);
    first.task_id = Some("T1".to_owned());
    let mut rename = batch("cli:sess-1", "rivet", "cli");
    rename.title = Some(renamed.to_owned());
    let mut clear = batch("cli:sess-1", "rivet", "cli");
    clear.settings = Some(object("{}")?);
    Ok(Plan {
        steps: vec![
            (
                "capture with title/settings/task_id",
                Action::Capture(first),
            ),
            (
                "capture no messages",
                Action::Capture(batch("cli:sess-1", "rivet", "cli")),
            ),
            ("loadSessionSettings", Action::Skip(SETTINGS_SKIP)),
            ("rename", Action::Capture(rename)),
            ("clear title with null, settings {}", Action::Capture(clear)),
        ],
        deviations: vec![Deviation {
            table: "ros_conversations",
            row: 0,
            column: "title",
            value: Json::String(renamed.to_owned()),
            reason: "wire CaptureBatchRequest.title is Option<String>: an explicit null is the same as absent, and the B3 schema rejects title:null, so the title is kept",
        }],
        ..Plan::default()
    })
}

fn timestamps_and_finalize() -> TestResult<Plan> {
    let session =
        |updated_at: Option<&str>, created_at: Option<&str>| -> TestResult<CaptureBatchRequest> {
            let mut capture = batch("old-task", "rivet", "cli");
            capture.updated_at = updated_at.map(instant).transpose()?;
            capture.created_at = created_at.map(instant).transpose()?;
            Ok(capture)
        };
    let mut first = session(
        Some("2025-01-01T00:00:00.000Z"),
        Some("2020-01-01T00:00:00.000Z"),
    )?;
    first.messages.push(message("e1", Role::User, "then"));
    let mut finalize = session(Some("2025-03-01T00:00:00Z"), None)?;
    finalize.finalize = Some(true);
    Ok(Plan {
        steps: vec![
            ("old created/updated", Action::Capture(first)),
            (
                "older updated_at",
                Action::Capture(session(Some("2024-06-01T00:00:00Z"), None)?),
            ),
            (
                "later updated_at with offset",
                Action::Capture(session(Some("2025-03-01T01:00:00+01:00"), None)?),
            ),
            ("finalize", Action::Capture(finalize.clone())),
            (
                "finalize again (already inactive)",
                Action::Capture(finalize),
            ),
            ("bad created_at", Action::Skip(BAD_STAMP)),
            ("bad updated_at", Action::Skip(BAD_STAMP)),
            ("bad message created_at", Action::Skip(BAD_STAMP)),
        ],
        ..Plan::default()
    })
}

fn pending_tool_fill() -> TestResult<Plan> {
    let tool = |metadata: &str, result: Option<&str>| -> TestResult<CaptureBatchRequest> {
        let mut capture = batch("cli:sess-1", "rivet", "cli");
        let mut call = message("cowork:s:tool:tu1", Role::Tool, "[tool call] Bash");
        call.tool_name = Some("Bash".to_owned());
        call.tool_args = Some(object(r#"{"command":"ls"}"#)?);
        call.tool_result = result.map(str::to_owned);
        call.metadata = Some(object(metadata)?);
        capture.messages.push(call);
        Ok(capture)
    };
    let filled = r#"{"tool_use_id":"tu1","pending_result":false}"#;
    Ok(Plan {
        steps: vec![
            (
                "pending tool row",
                Action::Capture(tool(
                    r#"{"tool_use_id":"tu1","pending_result":true}"#,
                    None,
                )?),
            ),
            ("result arrives", Action::Capture(tool(filled, Some("ok"))?)),
            (
                "result again (no change)",
                Action::Capture(tool(filled, Some("ok"))?),
            ),
        ],
        ..Plan::default()
    })
}

fn cowork_claim() -> TestResult<Plan> {
    let capture = |source: &str, rows: &[(&str, Role, &str)]| -> TestResult<CaptureBatchRequest> {
        let metadata = format!(r#"{{"source":"{source}"}}"#);
        let mut capture = batch("cowork:sess", "rivet-cowork", "cowork");
        capture.settings = Some(object(&metadata)?);
        for (event_id, role, content) in rows {
            let mut row = message(event_id, *role, content);
            row.metadata = Some(object(&metadata)?);
            capture.messages.push(row);
        }
        Ok(capture)
    };
    Ok(Plan {
        steps: vec![
            (
                "hook rows",
                Action::Capture(capture(
                    "cowork-hook",
                    &[
                        ("cowork:sess:hook:prompt-1", Role::User, "ship it"),
                        ("cowork:sess:hook:reply-1", Role::Assistant, "shipped"),
                    ],
                )?),
            ),
            (
                "transcript rows claim the hook rows",
                Action::Capture(capture(
                    "cowork-transcript",
                    &[
                        ("cowork:sess:uuid-prompt", Role::User, "ship it"),
                        ("cowork:sess:uuid-reply", Role::Assistant, "shipped"),
                    ],
                )?),
            ),
            (
                "transcript again",
                Action::Capture(capture(
                    "cowork-transcript",
                    &[
                        ("cowork:sess:uuid-prompt", Role::User, "ship it"),
                        ("cowork:sess:uuid-new", Role::User, "ship it"),
                    ],
                )?),
            ),
        ],
        ..Plan::default()
    })
}

fn capped_text() -> TestResult<Plan> {
    let boundary = format!("{}\u{1F600}xxxx", "x".repeat(15_999));
    let kept: String = boundary.chars().take(16_000).collect();
    let full = boundary.chars().count();
    let mut capture = batch("cli:sess-1", "rivet", "cli");
    let mut big = message("big", Role::User, &"x".repeat(20_000));
    big.created_at = Some(instant("2026-10-04T08:00:00.000Z")?);
    let mut surrogate = message("sur", Role::Tool, &boundary);
    surrogate.tool_name = Some("Read".to_owned());
    surrogate.tool_result = Some(boundary);
    surrogate.created_at = Some(instant("2026-10-04T10:00:01.500+02:00")?);
    capture.messages = vec![
        big,
        surrogate,
        message("exact", Role::Assistant, &"y".repeat(16_000)),
    ];
    let reason = "text lengths count chars, never UTF-16 units: the emoji at the 16000 boundary is one char and is kept whole";
    let cell = |column: &'static str, value: Json| Deviation {
        table: "ros_messages",
        row: 1,
        column,
        value,
        reason,
    };
    Ok(Plan {
        steps: vec![("big + surrogate", Action::Capture(capture))],
        deviations: vec![
            cell("content", Json::String(kept.clone())),
            cell("tool_result", Json::String(kept)),
            cell(
                "metadata",
                Json::String(format!(
                    r#"{{"event_id":"sur","full_content_length":{full},"truncated":true,"full_tool_result_length":{full}}}"#
                )),
            ),
        ],
        ..Plan::default()
    })
}

fn owner_stamp() -> TestResult<Plan> {
    Ok(Plan {
        owner: Some(" alice "),
        steps: vec![
            (
                "capture",
                Action::Capture(deploy_batch("cli:sess-1", "rivet")?),
            ),
            (
                "append",
                Action::Append(entry("cli:sess-9", "cli", Role::User, "owned row")),
            ),
        ],
        ..Plan::default()
    })
}

fn appends() -> TestResult<Plan> {
    let task = "00000000-0000-4000-8000-300000000004";
    let mut tool = entry("cli:alice", "cli", Role::Tool, "ran");
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(object(r#"{"command":"ls","n":1}"#)?);
    tool.tool_result = Some("a\nb".to_owned());
    tool.metadata = Some(object(r#"{"k":"v"}"#)?);
    tool.created_at = Some(instant("2026-10-10T11:00:00.000Z")?);
    let mut tagged = entry("cli:meta-task", "cli", Role::User, "tagged by metadata");
    tagged.metadata = Some(object(&format!(r#"{{"taskId":"{task}"}}"#))?);
    let alice = |column: &'static str, value: Json| Deviation {
        table: "ros_conversations",
        row: 0,
        column,
        value,
        reason: SETTINGS_SKIP,
    };
    Ok(Plan {
        steps: vec![
            (
                "append user",
                Action::Append(entry("cli:alice", "cli", Role::User, "first message")),
            ),
            ("append with createdAt + tool data", Action::Append(tool)),
            (
                "append task session key",
                Action::Append(entry(
                    &format!("task:{task}"),
                    "task",
                    Role::User,
                    "task work",
                )),
            ),
            ("append metadata.taskId", Action::Append(tagged)),
            (
                "append non-uuid task key",
                Action::Append(entry("task:not-a-uuid", "task", Role::User, "legacy task")),
            ),
            (
                "append heartbeat",
                Action::Append(entry("heartbeat:rivet", "heartbeat", Role::System, "beat")),
            ),
        ]
        .into_iter()
        .chain(append_skips())
        .collect(),
        deviations: vec![
            alice("settings", Json::String("{}".to_owned())),
            alice("task_id", Json::Null),
            alice(
                "updated_at",
                Json::String("2026-10-10T12:00:01.000Z".to_owned()),
            ),
        ],
        ..Plan::default()
    })
}

fn append_skips() -> Vec<(&'static str, Action)> {
    let not_in_store = "MemoryEntry carries no user and the store has one owner: other-user refusal is outside this API";
    vec![
        (
            "append no channel",
            Action::Skip(
                "MemoryEntry.channel is a required String; an unbound JavaScript undefined cannot be expressed",
            ),
        ),
        (
            "append other user (refused, logs once)",
            Action::Skip(not_in_store),
        ),
        (
            "append other user again (no second log)",
            Action::Skip(not_in_store),
        ),
        (
            "append bad role (CHECK)",
            Action::Skip("Role is a closed enum, so an unknown role cannot reach the store"),
        ),
        ("getSessionHistory", Action::Skip(HISTORY_SKIP)),
        ("getSessionHistory limit 1", Action::Skip(HISTORY_SKIP)),
        ("getSessionHistory other user", Action::Skip(HISTORY_SKIP)),
        ("getTaskHistory uuid", Action::Skip(HISTORY_SKIP)),
        ("getTaskHistory legacy", Action::Skip(HISTORY_SKIP)),
        ("saveSessionSettings", Action::Skip(SETTINGS_SKIP)),
        ("loadSessionSettings", Action::Skip(SETTINGS_SKIP)),
        ("loadSessionSettings missing", Action::Skip(SETTINGS_SKIP)),
        (
            "loadSessionSettings other user",
            Action::Skip(SETTINGS_SKIP),
        ),
        ("associateTask", Action::Skip(SETTINGS_SKIP)),
        (
            "search other user",
            Action::Skip("search is outside this slice"),
        ),
        (
            "getContextForTurn other user",
            Action::Skip("turn context is outside this slice"),
        ),
        ("isHealthy", Action::Skip("health is outside this slice")),
        (
            "append after close",
            Action::Skip("the store has no close; dropping it is the close"),
        ),
    ]
}

fn plan(name: &str) -> TestResult<Option<Plan>> {
    Ok(Some(match name {
        "re-sent batch is skipped event by event" => resent_batch()?,
        "same event id under another session or agent is a different message" => {
            same_event_elsewhere()?
        }
        "title, settings and task id kept unless supplied" => kept_unless_supplied()?,
        "source timestamps kept; updated_at only moves forward; finalize" => {
            timestamps_and_finalize()?
        }
        "pending tool result filled in place" => pending_tool_fill()?,
        "cowork hook prompt rewritten onto the transcript id, once" => cowork_claim()?,
        "UTC timestamps; oversized and surrogate-boundary text capped with metadata" => {
            capped_text()?
        }
        "owner stamp (userId alice)" => owner_stamp()?,
        "append, histories, settings, task ids, heartbeat, other users" => appends()?,
        _ => return Ok(None),
    }))
}

async fn replay(scenario: &mut Scenario, plan: Plan) -> TestResult<Vec<String>> {
    let mut notes = Vec::new();
    let fresh = fresh_store()?;
    let owner = Owner::new(plan.owner.unwrap_or(testkit::OWNER).trim().to_owned())?;
    let store = &fresh.store;
    let mut ids = Ids::default();
    assert_eq!(
        scenario.steps.len(),
        plan.steps.len(),
        "{}: step count",
        scenario.name
    );
    for (step, (label, action)) in scenario.steps.iter().zip(plan.steps) {
        assert_eq!(step.label, label, "{}: step order", scenario.name);
        let now = instant(&step.at)?;
        let at = format!("{} / {label}", scenario.name);
        match action {
            Action::Capture(request) => {
                match (store.capture(request, &owner, None, now).await, &step.error) {
                    (Ok(result), None) => {
                        let actual: Json = serde_json::to_value(&result)?;
                        let expected = step.result.clone().unwrap_or(Json::Null);
                        ids.same(&expected, &actual, &at)?;
                    }
                    (Err(error), Some(recorded)) => {
                        assert_eq!(error.to_string(), recorded.message, "{at}");
                    }
                    (outcome, recorded) => {
                        return Err(format!(
                            "{at}: outcome {outcome:?} vs recorded error {recorded:?}"
                        )
                        .into());
                    }
                }
            }
            Action::Append(entry) => {
                let appended = store.append_entries(vec![entry], &owner, now).await?;
                let actual = Json::Array(appended.into_iter().map(Json::String).collect());
                let expected = Json::Array(step.result.clone().into_iter().collect());
                ids.same(&expected, &actual, &at)?;
            }
            Action::Skip(reason) => notes.push(format!("skipped step {at}: {reason}")),
        }
    }
    for deviation in plan.deviations {
        notes.push(format!(
            "deviation {}: {} row {} {}: {}",
            scenario.name, deviation.table, deviation.row, deviation.column, deviation.reason
        ));
        let row = scenario
            .tables
            .get_mut(deviation.table)
            .and_then(|rows| rows.get_mut(deviation.row))
            .and_then(Json::as_object_mut)
            .ok_or_else(|| {
                format!(
                    "{}: no {} row {}",
                    scenario.name, deviation.table, deviation.row
                )
            })?;
        row.insert(deviation.column.to_owned(), deviation.value);
    }
    if plan.owner.is_none() {
        notes.push(format!(
            "deviation {}: owner_user_id is the capturing caller, where the recording ran without a user (CONTRACT note 88)",
            scenario.name
        ));
        for table in ["ros_conversations", "ros_messages"] {
            if let Some(rows) = scenario.tables.get_mut(table) {
                common::owned_by_the_caller(rows);
            }
        }
    }
    compare_tables(store, &scenario.tables, &mut ids, &scenario.name).await?;
    Ok(notes)
}

#[test]
fn skipped_timestamp_steps_are_the_plan() -> TestResult {
    let plan = timestamps_and_finalize()?;
    for name in ["bad created_at", "bad updated_at", "bad message created_at"] {
        let action = plan
            .steps
            .iter()
            .find(|(label, _)| *label == name)
            .map(|(_, action)| action)
            .ok_or_else(|| format!("plan is missing {name}"))?;
        match action {
            Action::Skip(reason) if *reason == BAD_STAMP => {}
            Action::Skip(reason) => {
                return Err(format!("{name} skipped for {reason}").into());
            }
            Action::Capture(_) | Action::Append(_) => {
                return Err(format!("{name} must skip with the stamp reason").into());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn capture_and_append_replay_the_recorded_scenarios() -> TestResult {
    let file: ScenarioFile = common::recording("capture.json")?;
    let mut replayed = Vec::new();
    for mut scenario in file.scenarios {
        if let Some((name, reason)) = SKIPPED_SCENARIOS
            .iter()
            .find(|(name, _)| *name == scenario.name)
        {
            eprintln!("skipped scenario {name}: {reason}");
            continue;
        }
        let plan = plan(&scenario.name)?
            .ok_or_else(|| format!("no replay plan for scenario {:?}", scenario.name))?;
        for note in replay(&mut scenario, plan).await? {
            eprintln!("{note}");
        }
        replayed.push(scenario.name);
    }
    assert_eq!(replayed.len(), 9);
    Ok(())
}

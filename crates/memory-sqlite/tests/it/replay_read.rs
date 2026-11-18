use crate::common;
use jiff::Timestamp;

use std::collections::HashMap;

use common::Ids;
use common::ScenarioFile;
use common::TestResult;
use common::batch;
use common::deploy_batch;
use common::fresh_store;
use common::instant;
use common::message;
use common::object;
use kernel::memory::BrowseFilter;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStatsResponse;
use kernel::memory::MemoryStore as _;
use memory_sqlite::SqliteStore;
use serde::Deserialize;
use testkit::Json;
use wire::gateway::memory::MemoryBrowseToolArgs;
use wire::message::Role;

const READ_CALLS: [&str; 9] = [
    "get_full message",
    "get_full tool message",
    "get_full missing",
    "browse default",
    "browse include tools asc",
    "browse window last_7d",
    "browse empty",
    "stats",
    "stats agent",
];

fn helper_batch() -> TestResult<kernel::memory::CaptureBatchRequest> {
    let mut helper = batch("cli:sess-2", "helper", "cli");
    helper.title = Some("Helper session".to_owned());
    let mut read_call = message("h1", Role::Assistant, "reading the deploy notes");
    read_call.tool_name = Some("Read".to_owned());
    read_call.tool_args = Some(object(
        r#"{"file_path":"/home/user/work/AcmeKit/DEPLOY.md"}"#,
    )?);
    let mut read_result = message("h2", Role::Tool, "");
    read_result.tool_name = Some("Read".to_owned());
    read_result.tool_result = Some("deploy from the release branch".to_owned());
    let mut bash = message("h3", Role::Tool, "");
    bash.tool_name = Some("Bash".to_owned());
    bash.tool_result = Some("ok".to_owned());
    helper.messages = vec![read_call, read_result, bash];
    Ok(helper)
}

async fn stats_json(store: &SqliteStore, agent: Option<&str>) -> TestResult<Json> {
    let stats: MemoryStatsResponse = store.stats_for(agent.map(str::to_owned)).await?;
    Ok(serde_json::to_value(stats)?)
}

#[tokio::test]
async fn stats_replay_the_recorded_scenarios() -> TestResult {
    let file: ScenarioFile = common::recording("stats-health.json")?;
    for (name, seeded) in [
        ("empty store", false),
        ("captured, no embedding endpoint", true),
    ] {
        let scenario = file
            .scenarios
            .iter()
            .find(|scenario| scenario.name == name)
            .ok_or_else(|| format!("missing scenario {name}"))?;
        let fresh = fresh_store()?;
        if seeded {
            let at = instant(common::RECORDED_START)?;
            for batch in [deploy_batch("cli:sess-1", "rivet")?, helper_batch()?] {
                fresh
                    .store
                    .capture(batch, &testkit::owner()?, None, at)
                    .await?;
            }
        }
        let mut ids = Ids::default();
        for step in &scenario.steps {
            let agent = match step.label.as_str() {
                "stats" => None,
                "stats agent helper" => Some("helper"),
                "stats agent nobody" => Some("nobody"),
                _ => continue,
            };
            let expected = step.result.clone().ok_or("recorded stats result")?;
            let actual = stats_json(&fresh.store, agent).await?;
            ids.same(&expected, &actual, &format!("{name} / {}", step.label))?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    label: String,
    tool: String,
    args: Json,
    output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolsRecording {
    calls: Vec<ToolCall>,
    tables: HashMap<String, Vec<Json>>,
}

const LEAF: &str = "The team decided to deploy the AcmeKit service with a blue-green rollout. The release branch is cut on Mondays, the deploy script runs from the release branch, and a health check on staging.example.com gates the traffic switch. Rollback is a traffic switch back to the previous colour.";

async fn seed_tools_store(store: &SqliteStore, now: Timestamp) -> TestResult<String> {
    let captured = store
        .capture(
            deploy_batch("cli:sess-1", "rivet")?,
            &testkit::owner()?,
            None,
            now,
        )
        .await?;
    let conversation = captured.conversation_id;
    common::execute(
        store,
        &format!(
            "INSERT INTO ros_summaries
               (id, conversation_id, parent_id, depth, content, kind, message_count,
                earliest_at, latest_at, model, created_at)
             VALUES
               ('00000000-0000-4000-8000-600000000001', '{conversation}', NULL, 1,
                'Branch summary of the deploy talk', 'branch', 3,
                '2026-10-04T08:00:00.000Z', '2026-10-10T12:00:00.000Z', 'fake-chat',
                '2026-10-10T12:00:00.000Z'),
               ('00000000-0000-4000-8000-600000000002', '{conversation}',
                '00000000-0000-4000-8000-600000000001', 0, '{LEAF}', 'leaf', 3,
                '2026-10-04T08:00:00.000Z', '2026-10-10T12:00:00.000Z', NULL,
                '2026-10-10T12:00:00.000Z');"
        ),
    )
    .await?;
    Ok(conversation)
}

fn seeded_rows(recording: &ToolsRecording) -> TestResult<(Json, Json)> {
    let conversations = recording
        .tables
        .get("ros_conversations")
        .ok_or("recorded conversations")?;
    let seeded = conversations
        .iter()
        .find(|row| row["session_key"] == "cli:sess-1")
        .ok_or("recorded cli:sess-1 conversation")?;
    let messages = recording
        .tables
        .get("ros_messages")
        .ok_or("recorded messages")?
        .iter()
        .filter(|row| row["conversation_id"] == seeded["id"])
        .map(|row| {
            let mut row = row.clone();
            if let Some(fields) = row.as_object_mut() {
                fields.insert("access_count".to_owned(), Json::from(0));
                fields.insert("last_accessed_at".to_owned(), Json::Null);
            }
            row
        })
        .collect();
    Ok((Json::Array(vec![seeded.clone()]), Json::Array(messages)))
}

#[derive(Debug, PartialEq)]
struct BrowseLine {
    created_at: Timestamp,
    agent: String,
    role: String,
    tool_name: Option<String>,
    id: String,
    content: String,
}

fn browse_lines(output: &str) -> TestResult<Vec<BrowseLine>> {
    if output == "No messages in that range." {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = output.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((stamp, rest)) = line
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
        else {
            continue;
        };
        let (who, id) = rest.rsplit_once(" id=").ok_or("browse line id")?;
        let (who, tool_name) = match who.split_once(" (") {
            Some((who, tool)) => (who, Some(tool.trim_end_matches(')').to_owned())),
            None => (who, None),
        };
        let (agent, role) = who.split_once('/').ok_or("browse line agent/role")?;
        found.push(BrowseLine {
            created_at: instant(stamp)?,
            agent: agent.to_owned(),
            role: role.to_owned(),
            tool_name,
            id: id.to_owned(),
            content: lines.get(index + 1).copied().unwrap_or("").to_owned(),
        });
    }
    Ok(found)
}

fn entry_line(entry: &MemoryEntry, ids: &Ids) -> TestResult<BrowseLine> {
    let id = entry.id.clone().ok_or("browsed entry id")?;
    Ok(BrowseLine {
        created_at: entry.created_at.ok_or("browsed entry created_at")?,
        agent: entry.agent.clone(),
        role: entry.role.to_string(),
        tool_name: entry.tool_name.clone(),
        id: ids.recorded(&id)?,
        content: entry.content.clone(),
    })
}

fn browse_filter(args: &Json) -> TestResult<BrowseFilter> {
    let args: MemoryBrowseToolArgs = serde_json::from_value(args.clone())?;
    Ok(BrowseFilter {
        conversation_id: None,
        agent: args.agent,
        role: None,
        tool_name: None,
        tag: None,
        window: args
            .window
            .map(|window| window.to_string().parse())
            .transpose()?,
        since: None,
        before: None,
        include_tools: args.include_tools == Some(true),
        order: match args.order.unwrap_or_default() {
            wire::gateway::memory::MemoryBrowseOrder::Asc => MemoryBrowseOrder::Asc,
            wire::gateway::memory::MemoryBrowseOrder::Desc => MemoryBrowseOrder::Desc,
        },
        limit: Limit::clamped(args.limit, 50, 1..=200),
    })
}

fn field<'a>(output: &'a str, prefix: &str) -> TestResult<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .ok_or_else(|| format!("no {prefix:?} line in {output:?}").into())
}

fn check_get_full(entry: &MemoryEntry, output: &str) -> TestResult {
    let (header, body) = output.split_once("\n\n").ok_or("get_full body")?;
    let who = field(header, "Agent: ")?;
    assert_eq!(
        who,
        format!(
            "{}  Role: {}  Channel: {}",
            entry.agent, entry.role, entry.channel
        )
    );
    assert_eq!(
        Some(instant(field(header, "Created: ")?)?),
        entry.created_at
    );
    assert!(field(header, "Conversation: ")?.ends_with(&format!("(session {})", entry.session_id)));
    match &entry.tool_name {
        Some(tool) => {
            assert_eq!(field(header, "Tool: ")?, tool);
            let args: Option<wire::JsonObject> =
                Some(serde_json::from_str(field(header, "Tool args: ")?)?);
            assert_eq!(args, entry.tool_args);
        }
        None => assert!(!header.contains("\nTool: ")),
    }
    match body.split_once("\n\nTool result:\n") {
        Some((content, result)) => {
            assert_eq!(content.trim_end_matches('\n'), entry.content);
            assert_eq!(Some(result), entry.tool_result.as_deref());
        }
        None => assert_eq!(body, entry.content),
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct StatsText {
    conversations: u64,
    messages: u64,
    embedded: u64,
    tool_calls: u64,
    summaries: u64,
    queue: u64,
    failed: u64,
    top_tools: Vec<(String, u64)>,
}

fn number(text: &str) -> TestResult<u64> {
    Ok(text.trim().parse()?)
}

fn stats_text(output: &str) -> TestResult<StatsText> {
    let messages = field(output, "Messages: ")?;
    let (total, rest) = messages.split_once(" (").ok_or("messages line")?;
    let (embedded, rest) = rest.split_once(" embedded, ").ok_or("embedded count")?;
    let tool_calls = rest.trim_end_matches(" tool calls)");
    let embeddings = field(output, "Embeddings: ")?;
    let (_, queue) = embeddings.rsplit_once("queue ").ok_or("queue count")?;
    let (queue, failed) = queue.split_once(", failed ").ok_or("failed count")?;
    let top_tools = match field(output, "Top tools: ") {
        Ok(list) => list
            .split(", ")
            .map(|item| {
                let (tool, used) = item.split_once(" (").ok_or("top tool")?;
                Ok((tool.to_owned(), number(used.trim_end_matches(')'))?))
            })
            .collect::<TestResult<Vec<_>>>()?,
        Err(_) => Vec::new(),
    };
    Ok(StatsText {
        conversations: number(field(output, "Conversations: ")?)?,
        messages: number(total)?,
        embedded: number(embedded)?,
        tool_calls: number(tool_calls)?,
        summaries: number(field(output, "Summaries: ")?)?,
        queue: number(queue)?,
        failed: number(failed)?,
        top_tools,
    })
}

fn stats_of(stats: &MemoryStatsResponse) -> StatsText {
    StatsText {
        conversations: stats.conversations,
        messages: stats.messages,
        embedded: stats.embedded_messages,
        tool_calls: stats.tool_calls,
        summaries: stats.summaries,
        queue: stats.embed_queue_depth,
        failed: stats.failed_embeddings,
        top_tools: stats
            .top_tools
            .iter()
            .map(|top| (top.tool.clone(), top.count))
            .collect(),
    }
}

async fn replay_read_call(
    store: &SqliteStore,
    call: &ToolCall,
    output: &str,
    ids: &Ids,
    now: Timestamp,
) -> TestResult {
    match call.tool.as_str() {
        "memory_get_full" => {
            let recorded = call
                .args
                .get("id")
                .and_then(Json::as_str)
                .ok_or("get_full id")?;
            let id = ids.actual(recorded).unwrap_or_else(|_| recorded.to_owned());
            match store.get_full(&id).await? {
                Some(entry) => check_get_full(&entry, output)?,
                None => assert_eq!(output, format!("No message or summary with id {recorded}.")),
            }
        }
        "memory_browse" => {
            let entries = store.browse_at(browse_filter(&call.args)?, now).await?;
            let actual = entries
                .iter()
                .map(|entry| entry_line(entry, ids))
                .collect::<TestResult<Vec<_>>>()?;
            assert_eq!(actual, browse_lines(output)?, "{}", call.label);
        }
        "memory_stats" => {
            let agent = call.args.get("agent").and_then(Json::as_str);
            let stats = store.stats_for(agent.map(str::to_owned)).await?;
            assert_eq!(stats_of(&stats), stats_text(output)?, "{}", call.label);
        }
        other => return Err(format!("no replay for tool {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn read_tools_replay_browse_get_full_and_stats() -> TestResult {
    let recording: ToolsRecording = common::recording("tools.json")?;
    let fresh = fresh_store()?;
    let now = instant(common::RECORDED_START)?;
    seed_tools_store(&fresh.store, now).await?;
    let mut ids = Ids::default();
    let (mut conversations, mut messages) = seeded_rows(&recording)?;
    for rows in [&mut conversations, &mut messages] {
        if let Json::Array(rows) = rows {
            common::owned_by_the_caller(rows);
        }
    }
    ids.same(
        &conversations,
        &Json::Array(common::table(&fresh.store, "ros_conversations").await?),
        "tools seed: ros_conversations",
    )?;
    ids.same(
        &messages,
        &Json::Array(common::table(&fresh.store, "ros_messages").await?),
        "tools seed: ros_messages",
    )?;
    for label in READ_CALLS {
        let call = recording
            .calls
            .iter()
            .find(|call| call.label == label)
            .ok_or_else(|| format!("missing call {label}"))?;
        let output = call.output.as_deref().ok_or("recorded tool output")?;
        replay_read_call(&fresh.store, call, output, &ids, now).await?;
    }
    Ok(())
}

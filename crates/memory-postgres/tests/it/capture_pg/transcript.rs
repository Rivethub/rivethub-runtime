use super::{
    AGENT, CHANNEL, HerdrMetadata, HookOutcomeBody, IngestError, Json, PgPool, TranscriptMetadata,
    TranscriptSettings, Value, capped, encoded, now_stamp, parse_stamp, path_key, result_text,
    routed_user, str_at, task_id, text_parts, title_of,
};

struct Transcript {
    assistants: Vec<AssistantLine>,
    users: Vec<UserLine>,
    results: std::collections::HashMap<String, String>,
    ai_titles: Vec<String>,
    pr_url: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    stamps: Vec<chrono::DateTime<chrono::Utc>>,
    ingestable: bool,
}

struct AssistantLine {
    uuid: Option<String>,
    sidechain: bool,
    text: Option<String>,
    calls: Vec<(String, String, Value)>,
    at: Option<chrono::DateTime<chrono::Utc>>,
}

struct UserLine {
    uuid: Option<String>,
    content: String,
    at: Option<chrono::DateTime<chrono::Utc>>,
}

#[expect(clippy::too_many_lines, reason = "one transcript grammar per pass")]
fn parse_lines(lines: &[Value]) -> Transcript {
    let mut parsed = Transcript {
        assistants: Vec::new(),
        users: Vec::new(),
        results: std::collections::HashMap::new(),
        ai_titles: Vec::new(),
        pr_url: None,
        cwd: None,
        session_id: None,
        stamps: Vec::new(),
        ingestable: false,
    };
    for value in lines {
        if !value.is_object() {
            continue;
        }
        let kind = str_at(value, "type").unwrap_or_default();
        if let Some(session) = str_at(value, "sessionId")
            && parsed.session_id.is_none()
        {
            parsed.session_id = Some(session);
        }
        if let Some(cwd) = str_at(value, "cwd") {
            parsed.cwd = Some(cwd.trim().to_string());
        }
        match kind.as_str() {
            "ai-title" => {
                if let Some(title) = str_at(value, "aiTitle") {
                    parsed.ai_titles.push(title);
                }
            }
            "pr-link" => {
                parsed.pr_url = str_at(value, "prUrl");
            }
            "user" => {
                if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(message) = value.get("message") else {
                    continue;
                };
                let uuid = str_at(value, "uuid");
                let at = str_at(value, "timestamp").and_then(|t| parse_stamp(&t));
                if let Some(stamp) = at {
                    parsed.stamps.push(stamp);
                }
                match message.get("content") {
                    Some(Value::String(text)) => {
                        parsed.users.push(UserLine {
                            uuid,
                            content: text.clone(),
                            at,
                        });
                        parsed.ingestable = true;
                    }
                    Some(Value::Array(parts)) => {
                        for part in parts {
                            if str_at(part, "type").as_deref() == Some("tool_result") {
                                let id = str_at(part, "tool_use_id").unwrap_or_default();
                                let text = result_text(part.get("content").unwrap_or(&Value::Null));
                                parsed.results.insert(id, text);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let Some(message) = value.get("message") else {
                    continue;
                };
                let (text, calls) = text_parts(message.get("content").unwrap_or(&Value::Null));
                let at = str_at(value, "timestamp").and_then(|t| parse_stamp(&t));
                if let Some(stamp) = at {
                    parsed.stamps.push(stamp);
                }
                if text.is_some() || !calls.is_empty() {
                    parsed.ingestable = true;
                }
                parsed.assistants.push(AssistantLine {
                    uuid: str_at(value, "uuid"),
                    sidechain: value.get("isSidechain").and_then(Value::as_bool) == Some(true),
                    text,
                    calls,
                    at,
                });
            }
            _ => {}
        }
    }
    parsed
}

#[expect(
    clippy::too_many_lines,
    reason = "the transcript ingest is one narrative"
)]
pub(super) async fn ingest_transcript(
    pool: &PgPool,
    path: &str,
    lines: Option<&Vec<Value>>,
    opts: &Value,
) -> Result<Json, IngestError> {
    let Some(lines) = lines else {
        return Ok(encoded(HookOutcomeBody {
            session_key: path_key(path),
            conversation_id: String::new(),
            created: false,
            inserted: 0,
            already_stored: Some(0),
            skipped: Some("transcript file does not exist".to_string()),
        }));
    };
    let decoded: Vec<Value> = lines
        .iter()
        .filter_map(|line| {
            line.as_str()
                .and_then(|text| serde_json::from_str(text).ok())
        })
        .collect();
    let parsed = parse_lines(&decoded);
    if !parsed.ingestable {
        return Ok(encoded(HookOutcomeBody {
            session_key: path_key(path),
            conversation_id: String::new(),
            created: false,
            inserted: 0,
            already_stored: Some(0),
            skipped: Some("nothing to ingest".to_string()),
        }));
    }
    let session_id = str_at(opts, "sessionId").or_else(|| parsed.session_id.clone());
    let session_key = match str_at(opts, "sessionKeyOverride") {
        Some(override_key) => override_key,
        None => match &session_id {
            Some(id) => format!("claude-code:{id}"),
            None => path_key(path),
        },
    };
    let settings: wire::JsonObject = serde_json::from_value(encoded(TranscriptSettings {
        source: "claude-code-hook",
        file: path.to_string(),
        session_id: session_id.clone(),
        pr_url: parsed.pr_url.clone(),
        last_event: str_at(opts, "event"),
        last_ingest_at: now_stamp(),
        cwd: parsed.cwd.clone(),
    }))?;

    let fallback_title = parsed.users.first().map(|user| user.content.clone());
    let latest_ai_title = parsed.ai_titles.last().cloned();
    let title_for_create = latest_ai_title
        .clone()
        .or_else(|| fallback_title.as_deref().map(title_of));

    let mut writer = memory_postgres::CaptureWriter::begin(pool, &session_key).await?;
    let owner = routed_user(opts);
    let herdr = HerdrMetadata::from_opts(opts);
    let record = writer
        .ensure_conversation(&memory_postgres::ConversationEnsure {
            session_key: &session_key,
            agent: AGENT,
            channel: Some(CHANNEL),
            title: title_for_create.as_deref(),
            settings: &settings,
            task_id: task_id(opts).as_deref(),
            owner_user_id: &owner,
            created_at: parsed.stamps.first().copied(),
            updated_at: parsed.stamps.last().copied(),
            active: opts.get("markInactive").and_then(Value::as_bool) != Some(true),
        })
        .await?;

    let mut inserted = 0u64;
    let mut already_stored = 0u64;

    let stored_uuids = writer.stored_assistant_uuids(record.id).await?;
    for line in &parsed.assistants {
        let Some(text) = &line.text else { continue };
        if line
            .uuid
            .as_ref()
            .is_some_and(|uuid| stored_uuids.contains(uuid))
        {
            already_stored += 1;
            continue;
        }
        let metadata = TranscriptMetadata::build(
            line.uuid.as_deref(),
            Some(line.sidechain),
            None,
            None,
            herdr.clone(),
        )?;
        writer
            .insert_message(&memory_postgres::MessageRow {
                conversation_id: record.id,
                agent: AGENT,
                channel: CHANNEL,
                role: wire::message::Role::Assistant,
                content: &capped(text),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: &metadata,
                owner_user_id: &owner,
                created_at: line.at,
                batch_index: 0,
            })
            .await?;
        inserted += 1;
    }

    let mut supply: Vec<(Option<String>, Option<Value>)> = writer
        .stored_tool_calls(record.id)
        .await?
        .into_iter()
        .map(|(name, args)| (name, args.and_then(|text| serde_json::from_str(&text).ok())))
        .collect();
    for line in &parsed.assistants {
        for (call_id, name, input) in &line.calls {
            let result = parsed.results.get(call_id);
            if let Some(found) = supply.iter().position(|(stored, args)| {
                stored.as_deref().unwrap_or_default() == name
                    && args
                        .as_ref()
                        .is_some_and(|args| testkit::json_eq(args, input))
            }) {
                supply.swap_remove(found);
                already_stored += 1;
                continue;
            }
            let metadata = TranscriptMetadata::build(
                line.uuid.as_deref(),
                None,
                Some("PostToolUse"),
                Some(true),
                herdr.clone(),
            )?;
            let args: wire::JsonObject = serde_json::from_value(input.clone())?;
            writer
                .insert_message(&memory_postgres::MessageRow {
                    conversation_id: record.id,
                    agent: AGENT,
                    channel: CHANNEL,
                    role: wire::message::Role::Tool,
                    content: &format!("[tool call] {name}"),
                    tool_name: Some(name),
                    tool_args: Some(&args),
                    tool_result: result.map(String::as_str),
                    metadata: &metadata,
                    owner_user_id: &owner,
                    created_at: line.at,
                    batch_index: 0,
                })
                .await?;
            inserted += 1;
        }
    }

    let stored_users = writer.stored_user_contents(record.id).await?;
    let mut skipped_users = 0u64;
    for line in &parsed.users {
        let content = capped(&line.content);
        if stored_users.contains(&content) {
            skipped_users += 1;
            continue;
        }
        let metadata =
            TranscriptMetadata::build(line.uuid.as_deref(), None, None, Some(true), herdr.clone())?;
        writer
            .insert_message(&memory_postgres::MessageRow {
                conversation_id: record.id,
                agent: AGENT,
                channel: CHANNEL,
                role: wire::message::Role::User,
                content: &content,
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: &metadata,
                owner_user_id: &owner,
                created_at: line.at,
                batch_index: 0,
            })
            .await?;
        inserted += 1;
    }
    already_stored += skipped_users;
    already_stored += u64::try_from(
        stored_users
            .len()
            .saturating_sub(usize::try_from(skipped_users).unwrap_or(usize::MAX)),
    )
    .unwrap_or(0);

    let title_for_update = latest_ai_title.or_else(|| {
        if record.created {
            None
        } else {
            record.title.clone()
        }
    });
    let mark_inactive = opts.get("markInactive").and_then(Value::as_bool) == Some(true);
    writer
        .update_conversation(&memory_postgres::ConversationUpdate {
            id: record.id,
            updated_at: parsed.stamps.last().copied(),
            active: !mark_inactive,
            title: title_for_update.as_deref(),
            settings: &settings,
        })
        .await?;
    writer.commit().await?;

    Ok(encoded(HookOutcomeBody {
        session_key,
        conversation_id: record.id.to_string(),
        created: record.created,
        inserted,
        already_stored: Some(already_stored),
        skipped: None,
    }))
}

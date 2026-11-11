use super::{
    AGENT, CHANNEL, HerdrMetadata, HookEventMetadata, HookOutcomeBody, HookSettings, IngestError,
    Json, PgPool, Value, capped, encoded, now_stamp, result_object, routed_user, str_at, task_id,
    title_of, tool_result_of,
};

#[expect(clippy::too_many_lines, reason = "one hook narrative per call")]
pub(super) async fn ingest_hook_event(
    pool: &PgPool,
    payload: &Value,
    opts: &Value,
) -> Result<Json, IngestError> {
    let event = str_at(payload, "hook_event_name").unwrap_or_default();
    let Some(session_id) = str_at(payload, "session_id") else {
        return Ok(encoded(HookOutcomeBody {
            session_key: String::new(),
            conversation_id: String::new(),
            created: false,
            inserted: 0,
            already_stored: None,
            skipped: Some("no session_id".to_string()),
        }));
    };
    if event != "UserPromptSubmit" && event != "PostToolUse" {
        return Ok(encoded(HookOutcomeBody {
            session_key: format!("claude-code:{session_id}"),
            conversation_id: String::new(),
            created: false,
            inserted: 0,
            already_stored: None,
            skipped: Some(format!("unhandled event {event}")),
        }));
    }
    let session_key =
        str_at(opts, "sessionKeyOverride").unwrap_or_else(|| format!("claude-code:{session_id}"));

    let prompt = str_at(payload, "prompt");
    if event == "UserPromptSubmit" && prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
        return Ok(encoded(HookOutcomeBody {
            session_key: session_key.clone(),
            conversation_id: String::new(),
            created: false,
            inserted: 0,
            already_stored: None,
            skipped: Some("empty prompt".to_string()),
        }));
    }

    let title = match event.as_str() {
        "UserPromptSubmit" => title_of(prompt.as_deref().unwrap_or_default()),
        _ => "Claude Code session".to_string(),
    };
    let settings: wire::JsonObject = serde_json::from_value(encoded(HookSettings {
        source: "claude-code-hook",
        last_event: &event,
        last_ingest_at: now_stamp(),
        session_id: &session_id,
        cwd: str_at(payload, "cwd").map(|cwd| cwd.trim().to_string()),
    }))?;

    let mut writer = memory_postgres::CaptureWriter::begin(pool, &session_key).await?;
    let owner = routed_user(opts);
    let record = writer
        .ensure_conversation(&memory_postgres::ConversationEnsure {
            session_key: &session_key,
            agent: AGENT,
            channel: Some(CHANNEL),
            title: Some(&title),
            settings: &settings,
            task_id: task_id(opts).as_deref(),
            owner_user_id: &owner,
            created_at: None,
            updated_at: None,
            active: true,
        })
        .await?;

    let idempotency = str_at(opts, "idempotencyKey");
    let mut skipped: Option<String> = None;
    let mut inserted = 0u64;
    let duplicate = match &idempotency {
        Some(key) => writer.ingest_key_seen(record.id, key).await?,
        None => false,
    };
    if duplicate {
        skipped = Some("duplicate ingest_key".to_string());
    } else {
        let mut metadata = HookEventMetadata {
            source: "claude-code-hook",
            hook_event: event.clone(),
            ingest_key: None,
            herdr: HerdrMetadata::from_opts(opts),
        };
        if let Some(key) = &idempotency {
            metadata.ingest_key = Some(key.clone());
        }
        let metadata: wire::JsonObject = serde_json::from_value(encoded(metadata))?;
        let prompt_content;
        let tool_name;
        let tool_content;
        let args;
        let result;
        let row = if event.as_str() == "UserPromptSubmit" {
            prompt_content = capped(prompt.as_deref().unwrap_or_default());
            memory_postgres::MessageRow {
                conversation_id: record.id,
                agent: AGENT,
                channel: CHANNEL,
                role: wire::message::Role::User,
                content: &prompt_content,
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: &metadata,
                owner_user_id: &owner,
                created_at: None,
                batch_index: 0,
            }
        } else {
            tool_name = str_at(payload, "tool_name").unwrap_or_else(|| "unknown".into());
            tool_content = format!("[tool call] {tool_name}");
            args = result_object(payload.get("tool_input").unwrap_or(&Value::Null))?;
            result = tool_result_of(payload);
            memory_postgres::MessageRow {
                conversation_id: record.id,
                agent: AGENT,
                channel: CHANNEL,
                role: wire::message::Role::Tool,
                content: &tool_content,
                tool_name: Some(&tool_name),
                tool_args: args.as_ref(),
                tool_result: result.as_deref(),
                metadata: &metadata,
                owner_user_id: &owner,
                created_at: None,
                batch_index: 0,
            }
        };
        writer.insert_message(&row).await?;
        inserted = 1;
    }
    writer.touch_settings(record.id, &settings).await?;
    writer.commit().await?;

    Ok(encoded(HookOutcomeBody {
        session_key,
        conversation_id: record.id.to_string(),
        created: record.created,
        inserted,
        already_stored: None,
        skipped,
    }))
}

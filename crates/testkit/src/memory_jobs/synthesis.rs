use kernel::memory::MemoryStore;
use kernel::memory::jobs::MemoryJobStore;
use kernel::memory::jobs::SynthesizeToolCall;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::model::ModelRequest;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::MessageContent;
use wire::message::Role;

use super::message;
use super::noted;
use crate::FakeProvider;
use crate::Script;
use crate::topics::Outcome;

pub async fn a_tool_sentence_records_the_model_request<S, H>(
    store: &S,
    system_prompt: &str,
    mut handle: H,
) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
    H: AsyncFnMut(&FakeProvider, &JsonObject) -> Result<(), String>,
{
    let mut args = JsonObject::default();
    args.insert("path", "/etc/service.conf")?;
    let id = message(
        store,
        noted("synth", Role::Assistant, "\n\t").tool("read", Some(args), None),
    )
    .await?;
    let payload = JsonObject::from_serialize(&SynthesizeToolCall { message_id: id })?;
    let provider = scripted("  Read the configuration at /etc/service.conf.  ");
    handle(&provider, &payload).await.map_err(text)?;
    assert_eq!(
        loaded(store, id).await?,
        "Read the configuration at /etc/service.conf."
    );
    let requests = provider.chat_requests().await;
    let request = requests.first().ok_or("the model was not called")?;
    recorded(
        request,
        system_prompt,
        "tool_name: read\ntool_args: {\"path\":\"/etc/service.conf\"}\n",
    )?;
    assert_eq!(requests.len(), 1);
    handle(&provider, &payload).await.map_err(text)?;
    assert_eq!(provider.chat_requests().await.len(), 1);
    Ok(())
}

pub async fn synthesis_drops_missing_populated_and_nameless_calls<S, H>(
    store: &S,
    mut handle: H,
) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
    H: AsyncFnMut(&FakeProvider, &JsonObject) -> Result<(), String>,
{
    let populated = message(
        store,
        noted("synth", Role::Assistant, "Existing content.").tool("read", None, None),
    )
    .await?;
    let nameless = message(store, noted("synth", Role::Assistant, "  ")).await?;
    let provider = scripted("Unused model response.");
    for id in [Uuid::nil(), populated, nameless] {
        let payload = JsonObject::from_serialize(&SynthesizeToolCall { message_id: id })?;
        handle(&provider, &payload).await.map_err(text)?;
    }
    assert_eq!(provider.chat_requests().await.len(), 0);
    let kept = store
        .tool_call(populated)
        .await?
        .ok_or("the populated call is missing")?;
    assert_eq!(kept.content, "Existing content.");
    Ok(())
}

pub async fn synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one<S, H>(
    store: &S,
    mut handle: H,
) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
    H: AsyncFnMut(&FakeProvider, &JsonObject) -> Result<(), String>,
{
    let id = message(
        store,
        noted("synth", Role::Assistant, "").tool("read", None, None),
    )
    .await?;
    let payload = JsonObject::from_serialize(&SynthesizeToolCall { message_id: id })?;
    let long = format!("{}.", "a".repeat(500));
    for answer in [
        "",
        "Ran it.",
        "The ASSISTANT read the file.",
        "Read the file without punctuation",
        long.as_str(),
        "😀😀😀😀.",
    ] {
        reject(store, &mut handle, id, &payload, answer).await?;
    }
    let unicode = format!("{}.", "😀".repeat(499));
    handle(&scripted(&unicode), &payload).await.map_err(text)?;
    assert_eq!(loaded(store, id).await?, unicode);
    Ok(())
}

async fn reject<S, H>(
    store: &S,
    handle: &mut H,
    id: Uuid,
    payload: &JsonObject,
    answer: &str,
) -> Outcome
where
    S: MemoryJobStore,
    H: AsyncFnMut(&FakeProvider, &JsonObject) -> Result<(), String>,
{
    match handle(&scripted(answer), payload).await {
        Err(error) => assert_eq!(error, "Empty synth response"),
        Ok(()) => return Err("an invalid sentence was stored".into()),
    }
    assert_eq!(loaded(store, id).await?, "");
    Ok(())
}

fn recorded(request: &ModelRequest, prompt: &str, user: &str) -> Outcome {
    assert_eq!(request.model, "summary-model");
    assert_eq!(request.max_tokens, Some(200));
    assert_eq!(request.options.temperature, Some(0.2));
    assert_eq!(request.options.enable_thinking, Some(false));
    let system = request
        .messages
        .first()
        .ok_or("the system turn is missing")?;
    let turn = request.messages.get(1).ok_or("the user turn is missing")?;
    assert_eq!(system.content, MessageContent::Text(prompt.to_owned()));
    assert_eq!(turn.content, MessageContent::Text(user.to_owned()));
    assert_eq!(request.messages.len(), 2);
    Ok(())
}

async fn loaded<S: MemoryJobStore>(
    store: &S,
    id: Uuid,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(store
        .tool_call(id)
        .await?
        .ok_or("the tool call is missing")?
        .content)
}

fn scripted(text: &str) -> FakeProvider {
    FakeProvider::new(vec![Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(text.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })])
}

fn text(error: String) -> Box<dyn std::error::Error> {
    error.into()
}

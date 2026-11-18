#[path = "../../../src/summarize/top_down.rs"]
mod top_down;

use kernel::memory::MemoryEntry;
use kernel::model::ModelProvider as _;
use std::collections::BTreeMap;
use wire::JsonObject;

use super::*;
use top_down::llm;
use top_down::plan::{self, ComposePlan, Meter, Source};
use top_down::prompt::{self, Mode, Prompt};
use top_down::settings::{self, Limits};

#[derive(Deserialize)]
struct Fixture {
    sessions: Vec<Session>,
    replies: Replies,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    role: Role,
    created_at: Timestamp,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<JsonObject>,
    tool_result: Option<String>,
}

impl From<Row> for MemoryEntry {
    fn from(row: Row) -> Self {
        Self {
            id: Some(row.id),
            role: row.role,
            created_at: Some(row.created_at),
            content: row.content,
            tool_name: row.tool_name,
            tool_args: row.tool_args,
            tool_result: row.tool_result,
            session_id: "fixture".to_owned(),
            agent: "fixture".to_owned(),
            channel: "fixture".to_owned(),
            metadata: None,
            provenance: None,
        }
    }
}

#[derive(Deserialize)]
struct Session {
    name: String,
    messages: Vec<Row>,
    calibration: Option<usize>,
    ratio: f64,
    dense_exact: bool,
    window: u32,
    chunk_tokens: u32,
    thinking: bool,
    rendered: String,
    tags: BTreeMap<String, Uuid>,
    render_keys: BTreeMap<Uuid, String>,
    chunks: Vec<ExpectedChunk>,
    spans: Vec<Vec<Uuid>>,
    total_tokens: usize,
    requests: Vec<Request>,
    part_records: Vec<String>,
}

#[derive(Deserialize)]
struct Request {
    step: String,
    body: RequestBody,
    editorial_ceiling: u32,
}

#[derive(Deserialize)]
struct RequestBody {
    model: String,
    messages: [ExpectedMessage; 2],
    temperature: f64,
    seed: u32,
    max_tokens: u32,
    stream: bool,
    stream_options: StreamOptions,
    chat_template_kwargs: TemplateKwargs,
}

#[derive(Deserialize)]
struct ExpectedMessage {
    role: Role,
    content: String,
}

#[derive(Deserialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Deserialize)]
struct TemplateKwargs {
    enable_thinking: bool,
}

#[derive(Deserialize)]
struct ExpectedChunk {
    text: String,
    estimate: usize,
    fragments: Vec<ExpectedFragment>,
}

#[derive(Deserialize)]
struct ExpectedFragment {
    id: Uuid,
    block_chars: [usize; 2],
    chunk_chars: [usize; 2],
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ExpectedSection {
    heading: String,
    chars: [usize; 2],
    bounds: [usize; 2],
    verified: bool,
}

#[derive(Deserialize)]
struct Replies {
    owners: Vec<usize>,
    cases: Vec<ReplyCase>,
}

#[derive(Deserialize)]
struct ReplyCase {
    name: String,
    text: String,
    finish: Option<String>,
    usage: Option<u64>,
    ceiling: u32,
    output: Option<String>,
    flags: u32,
    sections: Vec<ExpectedSection>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../../fixtures/top_down_prompts.json")).expect("fixture")
}

struct Playback {
    provider: Scripted,
    requests: std::vec::IntoIter<Request>,
}

impl Playback {
    async fn call(&mut self, step: &str, prompt: &Prompt, owner: &[Source]) -> String {
        let expected = self.requests.next().expect("expected request");
        assert_eq!(step, expected.step);
        let request = llm::model_request("fixture-chat", prompt);
        check_request(&request, expected.body, step);
        assert_eq!(
            prompt.editorial_ceiling, expected.editorial_ceiling,
            "{step}"
        );
        let reply = self.provider.chat(request).await.expect("scripted reply");
        let kind = if step == "session" {
            "TOP_DOWN_SESSION"
        } else if step == "final" {
            "TOP_DOWN_FINAL"
        } else if step.starts_with("part") {
            "TOP_DOWN_PART"
        } else {
            "TOP_DOWN_CARRY"
        };
        assert_eq!(
            self.provider
                .calls
                .lock()
                .await
                .last()
                .expect("captured call")
                .kind,
            kind
        );
        llm::parse_model_reply(&reply, prompt.editorial_ceiling, owner)
            .expect("nonempty scripted reply")
            .text
    }

    async fn compose(&mut self, step: &str, plan: ComposePlan, owner: &[Source]) -> String {
        let prompt = match plan {
            ComposePlan::Call(prompt) => prompt,
            ComposePlan::Reduce { groups, parent } => {
                let [left, right] = *groups;
                let left = Box::pin(self.compose(&format!("{step}.group0"), left, owner)).await;
                let right = Box::pin(self.compose(&format!("{step}.group1"), right, owner)).await;
                plan::after_reduction(parent, [&left, &right])
            }
        };
        self.call(step, &prompt, owner).await
    }
}

fn check_request(request: &ModelRequest, expected: RequestBody, step: &str) {
    let [system, user] = expected.messages;
    let system_text = super::script::system_text(request).expect("system text");
    let user_text = super::script::user_text(request).expect("user text");
    assert_eq!(
        system_text.as_bytes(),
        system.content.as_bytes(),
        "{step} system"
    );
    assert_eq!(user_text.as_bytes(), user.content.as_bytes(), "{step} user");
    assert_eq!(
        request.messages.first().map(|message| message.role),
        Some(system.role)
    );
    assert_eq!(
        request.messages.get(1).map(|message| message.role),
        Some(user.role)
    );
    assert_eq!(request.model, expected.model);
    assert_eq!(request.max_tokens, Some(expected.max_tokens));
    assert_eq!(request.options.temperature, Some(expected.temperature));
    assert_eq!(
        request.options.enable_thinking,
        Some(expected.chat_template_kwargs.enable_thinking)
    );
    assert_eq!(llm::SEED, expected.seed);
    assert_eq!(llm::STREAM, expected.stream);
    assert_eq!(llm::INCLUDE_USAGE, expected.stream_options.include_usage);
}

#[tokio::test]
async fn top_down_prompt_parity_requests() {
    for mut case in fixture().sessions {
        let limits = Limits::new(case.window, case.chunk_tokens, case.thinking).expect("limits");
        let rows: Vec<_> = std::mem::take(&mut case.messages)
            .into_iter()
            .map(MemoryEntry::from)
            .collect();
        let blocks = plan::render(&rows).expect("renderable source rows");
        let owner = check_rendering(&case, &blocks);
        let meter = case.calibration.map_or_else(Meter::uncalibrated, |count| {
            let sample: String = case.rendered.chars().take(200_000).collect();
            Meter::calibrate(&sample, count).expect("calibrated meter")
        });
        let expected_meter = Meter::new(case.ratio).expect("fixture ratio");
        assert!((meter.ratio() - expected_meter.ratio()).abs() < f64::EPSILON);
        assert_eq!(
            meter.estimate(&case.rendered),
            expected_meter.estimate(&case.rendered)
        );
        let plan = check_packing(&case, &blocks, meter, limits);

        let mut playback = Playback {
            provider: Scripted::default(),
            requests: case.requests.into_iter(),
        };
        let chunk_count = plan.chunks.len();
        if chunk_count == 1 {
            let chunk = plan.chunks.into_iter().next().expect("one chunk");
            let prompt = prompt::build(
                Mode::Session {
                    total_tokens: plan.total_tokens,
                },
                chunk.text,
                limits,
            );
            playback.call("session", &prompt, &owner).await;
        } else {
            let mut records = Vec::new();
            for (index, chunk) in plan.chunks.into_iter().enumerate() {
                let issued = index + 1;
                let mode = Mode::Part {
                    issued,
                    last: issued == chunk_count,
                };
                let prompt = prompt::build(mode, chunk.text, limits);
                let sources: Vec<_> = chunk
                    .fragments
                    .into_iter()
                    .map(|fragment| fragment.source)
                    .collect();
                records.push(
                    playback
                        .call(&format!("part{issued}"), &prompt, &sources)
                        .await,
                );
            }
            assert_eq!(records, case.part_records);
            let tree =
                plan::compose(&records, plan.total_tokens, meter, limits).expect("compose tree");
            playback.compose("final", tree, &owner).await;
        }
        assert!(playback.requests.next().is_none(), "{}", case.name);
    }
}

fn check_rendering(case: &Session, blocks: &[plan::Block]) -> Vec<Source> {
    let rendered = blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(rendered, case.rendered, "{}", case.name);
    let owner: Vec<_> = blocks.iter().map(|block| block.source.clone()).collect();
    let tags: BTreeMap<_, _> = owner
        .iter()
        .map(|source| (plan::tag(source.ordinal), source.id))
        .collect();
    assert_eq!(tags, case.tags, "{}", case.name);
    let render_keys: BTreeMap<_, _> = owner
        .iter()
        .map(|source| (source.id, hex::encode(source.render_key)))
        .collect();
    assert_eq!(render_keys, case.render_keys, "{}", case.name);
    owner
}

fn check_packing(
    case: &Session,
    blocks: &[plan::Block],
    meter: Meter,
    limits: Limits,
) -> plan::TranscriptPlan {
    let mut unavailable = |_: &str| Err(plan::PlanError::ExactCount);
    assert_eq!(
        plan::pack(
            blocks,
            Meter::uncalibrated(),
            limits,
            Some(&mut unavailable)
        )
        .expect_err("failed tokenizer"),
        plan::PlanError::ExactCount
    );
    let mut exact = |text: &str| {
        Ok(if case.dense_exact {
            text.chars().count()
        } else {
            meter.estimate(text)
        })
    };
    let plan = plan::pack(blocks, meter, limits, Some(&mut exact)).expect("chunks");
    if !case.dense_exact {
        let estimated = plan::pack(blocks, meter, limits, None).expect("estimate only");
        check_chunks(&estimated, &case.chunks, &case.name);
    }
    assert_eq!(plan.total_tokens, case.total_tokens, "{}", case.name);
    check_chunks(&plan, &case.chunks, &case.name);
    let spans: Vec<Vec<_>> = plan
        .chunks
        .iter()
        .map(|chunk| {
            chunk
                .fragments
                .iter()
                .map(|fragment| fragment.source.id)
                .collect()
        })
        .collect();
    assert_eq!(spans, case.spans, "{}", case.name);
    plan
}

fn check_chunks(plan: &plan::TranscriptPlan, expected: &[ExpectedChunk], case: &str) {
    assert_eq!(plan.chunks.len(), expected.len(), "{case}");
    for (chunk, expected) in plan.chunks.iter().zip(expected) {
        assert_eq!(chunk.text.as_bytes(), expected.text.as_bytes(), "{case}");
        assert_eq!(chunk.input_token_estimate, expected.estimate, "{case}");
        assert_eq!(chunk.fragments.len(), expected.fragments.len(), "{case}");
        for (fragment, expected) in chunk.fragments.iter().zip(&expected.fragments) {
            assert_eq!(fragment.source.id, expected.id, "{case}");
            assert_eq!(
                [fragment.block_chars.start, fragment.block_chars.end],
                expected.block_chars,
                "{case}"
            );
            assert_eq!(
                [fragment.chunk_chars.start, fragment.chunk_chars.end],
                expected.chunk_chars,
                "{case}"
            );
        }
    }
}

#[test]
fn top_down_prompt_parity_replies() {
    let replies = fixture().replies;
    let sources: Vec<_> = replies
        .owners
        .iter()
        .map(|&ordinal| Source {
            id: Uuid::from_u128(ordinal as u128),
            ordinal,
            created_at: Timestamp::UNIX_EPOCH,
            render_key: [0; 32],
        })
        .collect();
    for case in replies.cases {
        let result = llm::parse_reply(
            &case.text,
            case.finish.as_deref() == Some("stop"),
            case.usage,
            case.ceiling,
            &sources,
        );
        if let Some(text) = case.output {
            let reply = result.expect("nonempty reply");
            assert_eq!(reply.text, text, "{}", case.name);
            assert_eq!(reply.quality_flags, case.flags, "{}", case.name);
            let sections: Vec<_> = reply
                .sections
                .iter()
                .map(|section| ExpectedSection {
                    heading: section.heading.clone(),
                    chars: [section.chars.start, section.chars.end],
                    bounds: [section.source_first, section.source_last],
                    verified: section.range_verified,
                })
                .collect();
            assert_eq!(sections, case.sections, "{}", case.name);
        } else {
            assert_eq!(
                result.expect_err("empty reply").quality_flags,
                case.flags,
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn top_down_prompt_parity_limits_and_context_guard() {
    assert_eq!(settings::PART_PARALLELISM, 3);
    assert_eq!(settings::EVAL_CONTEXT_TOKENS, 262_144);
    assert_eq!(settings::FALLBACK_CONTEXT_TOKENS, 131_072);
    assert_eq!(settings::CHUNK_TOKENS, 90_000);
    assert!(Limits::new(262_144, 0, false).is_err());
    assert!(Limits::new(1_000, 900, false).is_err());
    assert!(Meter::new(0.0).is_err());
    let limits = Limits::new(262_144, 90_000, false).expect("limits");
    let short = prompt::build(Mode::Session { total_tokens: 1 }, String::new(), limits);
    assert_eq!(
        (short.target_words, short.max_words, short.max_tokens),
        (300, 225, 962)
    );
    let long = prompt::build(
        Mode::Final {
            count: 100,
            total_tokens: 250_000,
        },
        String::new(),
        limits,
    );
    assert_eq!(
        (long.target_words, long.max_words, long.max_tokens),
        (15_000, 15_000, 30_512)
    );
    let tight = Limits::new(6_000, 100, false).expect("limits");
    let too_long = "x".repeat(12_000);
    assert_eq!(
        plan::compose(&[too_long], 10, Meter::uncalibrated(), tight)
            .expect_err("oversized singleton"),
        plan::PlanError::ComposeContext
    );
}

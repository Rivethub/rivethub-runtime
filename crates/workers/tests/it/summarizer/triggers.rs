use super::*;
use kernel::model::ModelCallFailure;
use workers::OutageCause;
use workers::WorkerError;

#[derive(Debug, Deserialize)]
struct TriggerFile {
    accepted: Vec<TriggerCase>,
    rejected: Vec<RejectedCase>,
}

#[derive(Debug, Deserialize)]
struct TriggerCase {
    name: String,
    payload: wire::JsonObject,
}

#[derive(Debug, Deserialize)]
struct RejectedCase {
    name: String,
    payload: wire::JsonObject,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wired {
    conversation_id: Uuid,
    #[serde(default)]
    trigger_type: Option<SummaryTrigger>,
}

#[derive(Clone, Copy)]
enum WindowCase {
    Young,
    Idle,
    Stale,
    Full,
}

#[tokio::test]
async fn each_recorded_trigger_keeps_its_meaning() {
    let file = trigger_file();
    let now = at("2026-10-10T12:00:00Z");
    for case in &file.accepted {
        let wired = wired(&case.payload);
        for window_case in [
            WindowCase::Young,
            WindowCase::Idle,
            WindowCase::Stale,
            WindowCase::Full,
        ] {
            let summaries = run_case(
                &case.payload,
                sample(wired.conversation_id, now, window_case),
                now,
            )
            .await
            .expect("summarize");
            assert_eq!(
                summaries,
                summaries_of(wired.trigger_type, window_case),
                "{}",
                case.name
            );
        }
    }
    for case in &file.rejected {
        let error = run_case(
            &case.payload,
            sample(wired_id(), now, WindowCase::Full),
            now,
        )
        .await
        .expect_err("rejected trigger");
        let text = error.to_string();
        assert!(
            text.contains(&format!("unknown variant `{}`", case.name)),
            "{text}"
        );
    }
}

#[tokio::test]
async fn explicit_summarizes_a_short_tail_with_no_message_time() {
    let id = wired_id();
    let now = at("2026-10-10T12:00:00Z");
    let mut short = sample(id, now, WindowCase::Young);
    short.latest_message_at = None;
    assert_eq!(
        run_case(&payload_of(id, SummaryTrigger::Explicit), short, now)
            .await
            .expect("explicit"),
        1
    );
    let mut untouched = sample(id, now, WindowCase::Young);
    untouched.latest_message_at = None;
    assert_eq!(
        run_case(&payload_of(id, SummaryTrigger::Threshold), untouched, now)
            .await
            .expect("threshold"),
        0
    );
}

#[tokio::test]
async fn a_refused_model_is_not_a_failed_summary() {
    let id = wired_id();
    let now = at("2026-10-10T12:00:00Z");
    let store = Arc::new(MemStore::new(vec![sample(id, now, WindowCase::Full)]));
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let refused = ChatFailure {
        message: "the model server request failed",
        kind: std::io::ErrorKind::ConnectionRefused,
    };
    let env = SummarizeEnv {
        store: store.as_ref(),
        provider: &refused,
        queue: jobs.as_ref(),
        settings: &settings,
        now,
        cancel: &token,
    };
    let error = handle_summarize(&env, &payload_of(id, SummaryTrigger::SessionStale))
        .await
        .expect_err("refused");
    assert!(matches!(
        error,
        WorkerError::ProviderOutage {
            cause: OutageCause::Unreachable,
            retry_after: None
        }
    ));
    let shaped = ChatFailure {
        message: "connection refused",
        kind: std::io::ErrorKind::Other,
    };
    let env = SummarizeEnv {
        store: store.as_ref(),
        provider: &shaped,
        queue: jobs.as_ref(),
        settings: &settings,
        now,
        cancel: &token,
    };
    let error = handle_summarize(&env, &payload_of(id, SummaryTrigger::SessionIdle))
        .await
        .expect_err("shaped");
    assert!(matches!(
        error,
        WorkerError::ProviderOutage {
            cause: OutageCause::Unreachable,
            retry_after: None
        }
    ));
}

#[tokio::test]
async fn auth_throttling_and_server_errors_are_outages_not_failed_summaries() {
    let hint = Some(std::time::Duration::from_secs(30));
    let cases = [
        (Some(401), None, Some((OutageCause::Credentials, None))),
        (Some(403), None, Some((OutageCause::Credentials, None))),
        (None, None, Some((OutageCause::Credentials, None))),
        (Some(429), hint, Some((OutageCause::Throttled, hint))),
        (Some(500), None, Some((OutageCause::ServerError, None))),
        (Some(503), hint, Some((OutageCause::ServerError, hint))),
        (Some(400), None, None),
    ];
    for (status, retry_after, expected) in cases {
        let error = summarize_against(&StatusChat {
            status,
            retry_after,
        })
        .await;
        match (expected, error) {
            (
                Some((cause, hint)),
                WorkerError::ProviderOutage {
                    cause: got,
                    retry_after: got_hint,
                },
            ) => {
                assert_eq!(got, cause, "{status:?}");
                assert_eq!(got_hint, hint, "{status:?}");
            }
            (None, WorkerError::Llm { message }) => {
                assert_eq!(message, "LLM HTTP 400 (not retrying)");
            }
            (expected, error) => panic!("{status:?}: expected {expected:?}, got {error:?}"),
        }
    }
}

async fn summarize_against(provider: &StatusChat) -> WorkerError {
    let id = wired_id();
    let now = at("2026-10-10T12:00:00Z");
    let store = Arc::new(MemStore::new(vec![sample(id, now, WindowCase::Full)]));
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let env = SummarizeEnv {
        store: store.as_ref(),
        provider,
        queue: jobs.as_ref(),
        settings: &settings,
        now,
        cancel: &token,
    };
    handle_summarize(&env, &payload_of(id, SummaryTrigger::Explicit))
        .await
        .expect_err("the model fails")
}

struct StatusChat {
    status: Option<u16>,
    retry_after: Option<std::time::Duration>,
}

impl StatusChat {
    fn error(&self) -> ModelProviderError {
        let source = Box::new(std::io::Error::other("synthetic"));
        let failure = match self.status {
            Some(status) => ModelCallFailure::Http {
                status,
                retry_after: self.retry_after,
                source,
            },
            None => ModelCallFailure::Credentials { source },
        };
        ModelProviderError::Request {
            message: failure.to_string(),
            source: Box::new(failure),
        }
    }
}

#[async_trait]
impl ModelProvider for StatusChat {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(self.error())
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(self.error())
    }

    async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        Err(self.error())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

fn trigger_file() -> TriggerFile {
    let path = testkit::recording("workers/summary-triggers.json");
    let text = std::fs::read_to_string(&path).expect("trigger fixture");
    serde_json::from_str(&text).expect("trigger fixture")
}

fn wired(payload: &wire::JsonObject) -> Wired {
    let value = serde_json::to_value(payload).expect("payload value");
    serde_json::from_value(value).expect("trigger")
}

fn wired_id() -> Uuid {
    Uuid::parse_str("00000000-0000-4000-8000-0000000000a1").expect("id")
}

fn sample(id: Uuid, now: DateTime<Utc>, case: WindowCase) -> ConversationWindow {
    let (start, count) = match case {
        WindowCase::Young => (now, 2),
        WindowCase::Idle => (now - Duration::minutes(20), 6),
        WindowCase::Stale => (now - Duration::minutes(5_761), 2),
        WindowCase::Full => (now, 10),
    };
    window(id, "cli:triggers", "Triggers", messages(start, count))
}

async fn run_case(
    payload: &wire::JsonObject,
    sample: ConversationWindow,
    now: DateTime<Utc>,
) -> Result<u32, WorkerError> {
    let store = Arc::new(MemStore::new(vec![sample]));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let env = bind(&store, &script, &jobs, &settings, now, &token);
    handle_summarize(&env, payload)
        .await
        .map(|outcome| outcome.summaries)
}

fn summaries_of(trigger: Option<SummaryTrigger>, case: WindowCase) -> u32 {
    u32::from(match trigger {
        Some(SummaryTrigger::Explicit) => true,
        Some(SummaryTrigger::SessionIdle) => matches!(case, WindowCase::Idle | WindowCase::Full),
        Some(SummaryTrigger::SessionStale) => matches!(case, WindowCase::Stale | WindowCase::Full),
        Some(SummaryTrigger::Threshold) | None => matches!(case, WindowCase::Full),
    })
}

struct ChatFailure {
    message: &'static str,
    kind: std::io::ErrorKind,
}

#[async_trait]
impl ModelProvider for ChatFailure {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(ModelProviderError::Request {
            message: self.message.to_owned(),
            source: Box::new(std::io::Error::new(self.kind, "tcp")),
        })
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(ModelProviderError::Request {
            message: self.message.to_owned(),
            source: Box::new(std::io::Error::new(self.kind, "tcp")),
        })
    }

    async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        Err(ModelProviderError::Request {
            message: self.message.to_owned(),
            source: Box::new(std::io::Error::new(self.kind, "tcp")),
        })
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

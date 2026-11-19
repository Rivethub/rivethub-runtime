use std::future::Future;
use std::time::Duration;

use kernel::model::{LlmResponse, ModelChunk, ModelProviderError, ModelRequest};
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::time::{self, Instant};
use wire::stream::LlmUsage;

use super::Publisher;
use crate::WorkerError;
use crate::summarize::{SummarySettings, llm_error};

const LIVE_POLL: Duration = Duration::from_millis(250);

impl Publisher<'_> {
    pub(super) async fn chat_request(
        &self,
        request: ModelRequest,
    ) -> (Result<LlmResponse, WorkerError>, Option<LlmUsage>) {
        let settings = self.summary.settings;
        let provider = self.summary.provider;
        let observed = Mutex::new(None);
        let result = self
            .interruptible(watch_chat(settings, provider, request, &observed))
            .await;
        let usage = if let Ok(reply) = &result
            && let Some(usage) = &reply.usage
        {
            Some(usage.clone())
        } else {
            observed.into_inner()
        };
        (result, usage)
    }

    pub(super) async fn tokenizer_request<T>(
        &self,
        call: impl Future<Output = Result<T, ModelProviderError>>,
    ) -> Result<T, WorkerError> {
        let limit = self.summary.settings.first_token_timeout;
        self.interruptible(async {
            time::timeout(limit, call)
                .await
                .map_err(|_| timed_out(limit))?
                .map_err(llm_error)
        })
        .await
    }

    async fn interruptible<T>(
        &self,
        call: impl Future<Output = Result<T, WorkerError>>,
    ) -> Result<T, WorkerError> {
        tokio::select! {
            biased;
            () = self.summary.cancel.cancelled() => Err(WorkerError::Cancelled),
            result = self.wait_for_live() => result.and(Err(WorkerError::ConversionYield)),
            result = call => result,
        }
    }

    async fn wait_for_live(&self) -> Result<(), WorkerError> {
        if let Some((store, _)) = self.conversion {
            loop {
                time::sleep(LIVE_POLL).await;
                if store.live_pending().await? {
                    return Ok(());
                }
            }
        }
        std::future::pending().await
    }
}

async fn watch_chat(
    settings: &SummarySettings,
    provider: &dyn kernel::model::ModelProvider,
    request: ModelRequest,
    observed: &Mutex<Option<LlmUsage>>,
) -> Result<LlmResponse, WorkerError> {
    let first = settings.first_token_timeout;
    let idle = settings.inter_token_timeout;
    let overall = settings.request_timeout;
    let (progress, clock) = watch::channel((Instant::now() + first, first));
    let observe = |chunk: &ModelChunk| match chunk {
        ModelChunk::Text { delta } | ModelChunk::Reasoning { delta } if !delta.is_empty() => {
            progress.send_replace((Instant::now() + idle, idle));
        }
        ModelChunk::Usage { usage } => *observed.lock() = Some(usage.clone()),
        ModelChunk::Text { .. } | ModelChunk::Reasoning { .. } | ModelChunk::Done => {}
    };
    tokio::select! {
        biased;
        limit = stall(clock) => Err(timed_out(limit)),
        result = time::timeout(overall, provider.chat_observed(request, &observe)) => {
            result.map_err(|_| timed_out(overall))?.map_err(llm_error)
        }
    }
}

async fn stall(mut clock: watch::Receiver<(Instant, Duration)>) -> Duration {
    loop {
        let (deadline, limit) = *clock.borrow_and_update();
        tokio::select! {
            biased;
            changed = clock.changed() => {
                if changed.is_err() {
                    return limit;
                }
            }
            () = time::sleep_until(deadline) => return limit,
        }
    }
}

fn timed_out(limit: Duration) -> WorkerError {
    WorkerError::ModelTimeout {
        seconds: limit.as_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::model::{
        Embedding, FinishReason, LlmResponseKind, ModelObserver, ModelOptions, ModelProvider,
        ModelStreamItem,
    };
    use tokio::sync::mpsc::Receiver;

    struct Slow;

    #[async_trait::async_trait]
    impl ModelProvider for Slow {
        async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
            self.chat_observed(request, &|_| {}).await
        }

        async fn chat_observed(
            &self,
            _: ModelRequest,
            observe: &ModelObserver<'_>,
        ) -> Result<LlmResponse, ModelProviderError> {
            let delta = "Cobalt ".to_owned();
            let gap = Duration::from_secs(40);
            let mut tokens = time::interval_at(Instant::now() + gap, gap);
            for _ in 0..10 {
                tokens.tick().await;
                observe(&ModelChunk::Text {
                    delta: delta.clone(),
                });
            }
            Ok(LlmResponse {
                r#type: LlmResponseKind::Text,
                content: Some(delta.repeat(10)),
                tool_calls: None,
                usage: None,
                finish_reason: Some(FinishReason::Stop),
            })
        }

        async fn stream(
            &self,
            _: ModelRequest,
        ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
            unreachable!()
        }

        async fn embed(&self, _: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
            unreachable!()
        }

        async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
            unreachable!()
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            options: ModelOptions::default(),
            model: "cobalt-fixture".into(),
            messages: Vec::new(),
            thinking: None,
            max_tokens: Some(100),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn steady_output_runs_past_five_minutes() -> Result<(), WorkerError> {
        let settings = SummarySettings::standard("cobalt-fixture")?;
        let start = Instant::now();
        let response = watch_chat(&settings, &Slow, request(), &Mutex::new(None)).await?;
        assert_eq!(response.finish_reason, Some(FinishReason::Stop));
        assert_eq!(start.elapsed(), Duration::from_secs(400));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn overall_ceiling_applies_despite_steady_tokens() -> Result<(), WorkerError> {
        let mut settings = SummarySettings::standard("cobalt-fixture")?;
        settings.request_timeout = Duration::from_secs(200);
        let result = watch_chat(&settings, &Slow, request(), &Mutex::new(None)).await;
        assert!(matches!(
            result,
            Err(WorkerError::ModelTimeout { seconds: 200 })
        ));
        Ok(())
    }
}

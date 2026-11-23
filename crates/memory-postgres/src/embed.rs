use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use kernel::model::Embedding;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use parking_lot::Mutex;
use tokio::time::Instant;

pub use kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION;
pub(crate) use kernel::memory_core::compose_query_input;

const EMBED_TIMEOUT_DEFAULT_MS: u64 = 8_000;
const EMBED_TIMEOUT_MIN_MS: u64 = 500;
const EMBED_TIMEOUT_MAX_MS: u64 = 60_000;
const EMBED_CACHE_TTL: Duration = Duration::from_mins(10);
const EMBED_CACHE_CAPACITY: usize = 256;
const WARN_THROTTLE: Duration = Duration::from_secs(60);
pub(crate) const HNSW_EF_SEARCH_DEFAULT: u32 = 100;
const HNSW_EF_SEARCH_MIN: u32 = 10;
const HNSW_EF_SEARCH_MAX: u32 = 1000;

#[derive(Clone)]
pub struct QueryEmbed {
    provider: Arc<dyn ModelProvider>,
    instruction: String,
    timeout: Duration,
    ef_search: u64,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for QueryEmbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryEmbed")
            .field("instruction", &self.instruction)
            .field("timeout", &self.timeout)
            .field("ef_search", &self.ef_search)
            .finish_non_exhaustive()
    }
}

impl QueryEmbed {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        instruction: Option<String>,
        timeout_ms: Option<u64>,
        ef_search: Option<u64>,
    ) -> Self {
        let timeout_ms = timeout_ms
            .unwrap_or(EMBED_TIMEOUT_DEFAULT_MS)
            .clamp(EMBED_TIMEOUT_MIN_MS, EMBED_TIMEOUT_MAX_MS);
        let ef_search = ef_search
            .unwrap_or_else(|| u64::from(HNSW_EF_SEARCH_DEFAULT))
            .clamp(u64::from(HNSW_EF_SEARCH_MIN), u64::from(HNSW_EF_SEARCH_MAX));
        Self {
            provider,
            instruction: instruction.unwrap_or_else(|| DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned()),
            timeout: Duration::from_millis(timeout_ms),
            ef_search,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EmbedFailure {
    #[error(transparent)]
    Provider(#[from] ModelProviderError),
    #[error("bad response")]
    BadResponse,
    #[error("empty vector")]
    EmptyVector,
    #[error("timeout")]
    Timeout(#[source] tokio::time::error::Elapsed),
}

impl EmbedFailure {
    pub(crate) fn reason(&self) -> String {
        self.to_string()
    }
}

#[derive(Debug)]
pub(crate) enum EmbedAttempt {
    NotWired,
    Embedded {
        vector: Vec<f32>,
        ef_search: u64,
    },
    Failed {
        failure: EmbedFailure,
        elapsed_ms: u128,
    },
}

impl EmbedAttempt {
    pub(crate) fn vector(&self) -> Option<&Vec<f32>> {
        match self {
            Self::Embedded { vector, .. } => Some(vector),
            Self::NotWired | Self::Failed { .. } => None,
        }
    }
}

pub(crate) struct EmbedArm {
    wiring: QueryEmbed,
    cache: Mutex<CacheState>,
    last_warn: Mutex<Option<Instant>>,
    dropped: AtomicU64,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for EmbedArm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedArm")
            .field("instruction", &self.wiring.instruction)
            .field("timeout", &self.wiring.timeout)
            .field("ef_search", &self.wiring.ef_search)
            .field("cache", &self.cache)
            .field("dropped", &self.dropped)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct CacheState {
    entries: Vec<(String, Instant, Vec<f32>)>,
}

impl EmbedArm {
    pub(crate) fn new(wiring: QueryEmbed) -> Self {
        Self {
            wiring,
            cache: Mutex::new(CacheState::default()),
            last_warn: Mutex::new(None),
            dropped: AtomicU64::new(0),
        }
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn record_drop(&self, failure: &EmbedFailure, elapsed_ms: u128) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.warn_drop(failure, elapsed_ms);
    }

    pub(crate) fn warn_drop(&self, failure: &EmbedFailure, elapsed_ms: u128) {
        let now = Instant::now();
        let throttled = {
            let mut last = self.last_warn.lock();
            let due = last.is_none_or(|warned| now.duration_since(warned) >= WARN_THROTTLE);
            if due {
                *last = Some(now);
            }
            !due
        };
        if throttled {
            return;
        }
        tracing::warn!(
            "[memory-search] vector arm dropped: {} ({elapsed_ms}ms)",
            failure.reason()
        );
    }

    pub(crate) async fn query_vector(&self, text: &str) -> EmbedAttempt {
        let input = compose_query_input(&self.wiring.instruction, text);
        if let Some(vector) = self.cache_get(&input) {
            return EmbedAttempt::Embedded {
                vector,
                ef_search: self.wiring.ef_search,
            };
        }
        let started = Instant::now();
        let attempt = tokio::time::timeout(
            self.wiring.timeout,
            self.wiring.provider.embed(vec![input.clone()]),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis();
        let vector = match attempt {
            Err(source) => {
                return EmbedAttempt::Failed {
                    failure: EmbedFailure::Timeout(source),
                    elapsed_ms,
                };
            }
            Ok(Err(error)) => {
                return EmbedAttempt::Failed {
                    failure: EmbedFailure::from(error),
                    elapsed_ms,
                };
            }
            Ok(Ok(embeddings)) => match validated_vector(&embeddings) {
                Ok(vector) => vector,
                Err(failure) => {
                    return EmbedAttempt::Failed {
                        failure,
                        elapsed_ms,
                    };
                }
            },
        };
        self.cache_put(input, vector.clone());
        EmbedAttempt::Embedded {
            vector,
            ef_search: self.wiring.ef_search,
        }
    }

    fn cache_get(&self, input: &str) -> Option<Vec<f32>> {
        let now = Instant::now();
        let mut cache = self.cache.lock();
        let index = cache.entries.iter().rposition(|(key, _, _)| key == input)?;
        let (_, inserted, vector) = cache.entries.remove(index);
        if now.duration_since(inserted) > EMBED_CACHE_TTL {
            cache
                .entries
                .retain(|(_, inserted, _)| now.duration_since(*inserted) <= EMBED_CACHE_TTL);
            drop(cache);
            return None;
        }
        cache
            .entries
            .push((input.to_owned(), inserted, vector.clone()));
        drop(cache);
        Some(vector)
    }

    fn cache_put(&self, input: String, vector: Vec<f32>) {
        let now = Instant::now();
        let mut cache = self.cache.lock();
        cache.entries.retain(|(key, inserted, _)| {
            key != &input && now.duration_since(*inserted) <= EMBED_CACHE_TTL
        });
        while cache.entries.len() >= EMBED_CACHE_CAPACITY {
            cache.entries.remove(0);
        }
        cache.entries.push((input, now, vector));
        drop(cache);
    }
}

fn validated_vector(embeddings: &[Embedding]) -> Result<Vec<f32>, EmbedFailure> {
    match embeddings {
        [Embedding { vector }] if vector.is_empty() => Err(EmbedFailure::BadResponse),
        [Embedding { vector }] if vector.iter().all(|value| *value == 0.0) => {
            Err(EmbedFailure::EmptyVector)
        }
        [Embedding { vector }] => Ok(vector.clone()),
        _ => Err(EmbedFailure::BadResponse),
    }
}

impl crate::PostgresMemory {
    pub(crate) async fn embed_query(&self, text: &str) -> EmbedAttempt {
        match &self.embed {
            None => EmbedAttempt::NotWired,
            Some(arm) => arm.query_vector(text).await,
        }
    }

    pub(crate) fn vector_dropped(&self, failure: &EmbedFailure, elapsed_ms: u128) {
        if let Some(arm) = &self.embed {
            arm.record_drop(failure, elapsed_ms);
        }
    }

    pub(crate) fn vector_warn_only(&self, failure: &EmbedFailure, elapsed_ms: u128) {
        if let Some(arm) = &self.embed {
            arm.warn_drop(failure, elapsed_ms);
        }
    }

    #[must_use]
    pub fn vector_arm_dropped(&self) -> Option<u64> {
        self.embed.as_ref().map(EmbedArm::dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::FakeProvider;
    use testkit::Script;

    struct Harness {
        arm: EmbedArm,
        provider: Arc<FakeProvider>,
    }

    fn harness(script: Vec<Script>, timeout_ms: Option<u64>) -> Harness {
        let provider = Arc::new(FakeProvider::new(script));
        let arm = EmbedArm::new(QueryEmbed::new(provider.clone(), None, timeout_ms, None));
        Harness { arm, provider }
    }

    fn failure_of(attempt: EmbedAttempt) -> (String, u128) {
        match attempt {
            EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } => (failure.reason(), elapsed_ms),
            EmbedAttempt::Embedded { .. } | EmbedAttempt::NotWired => {
                panic!("the embed attempt unexpectedly succeeded")
            }
        }
    }

    #[tokio::test]
    async fn embed_failure_reasons_come_from_the_typed_provider_cases() {
        for (script, want) in [
            (
                vec![Script::EmbedHttpStatus(503)],
                "provider request failed: http 503",
            ),
            (
                vec![Script::EmbedNetwork],
                "provider request failed: connection refused",
            ),
            (vec![Script::Embed(vec![])], "bad response"),
            (
                vec![Script::Embed(vec![Embedding {
                    vector: vec![0.0; 8],
                }])],
                "empty vector",
            ),
        ] {
            let arm = harness(script, None).arm;
            let attempt = arm.query_vector("deploy").await;
            assert_eq!(failure_of(attempt).0, want);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_embed_times_out_at_the_clamped_timeout() {
        let arm = harness(vec![Script::EmbedHang], Some(100)).arm;
        let (reason, elapsed_ms) = failure_of(arm.query_vector("deploy").await);
        assert_eq!(reason, "timeout");
        assert_eq!(elapsed_ms, 500);
    }

    #[tokio::test]
    async fn identical_queries_hit_the_cache_once() {
        let Harness { arm, provider } = harness(
            vec![
                Script::Embed(vec![Embedding {
                    vector: vec![1.0, 0.5]
                }]);
                1
            ],
            None,
        );
        let first = arm.query_vector("deploy the release").await;
        let second = arm.query_vector(" deploy   the release ").await;
        let EmbedAttempt::Embedded { vector, .. } = first else {
            panic!("the first embed succeeds")
        };
        assert_eq!(second.vector(), Some(&vector));
        assert_eq!(provider.embed_requests().await.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cache_entries_expire_after_ten_minutes() {
        let Harness { arm, provider } = harness(
            vec![
                Script::Embed(vec![Embedding {
                    vector: vec![1.0, 0.5]
                }]);
                2
            ],
            None,
        );
        assert!(matches!(
            arm.query_vector("deploy").await,
            EmbedAttempt::Embedded { .. }
        ));
        tokio::time::advance(
            EMBED_CACHE_TTL
                .checked_sub(Duration::from_millis(1))
                .expect("a positive duration"),
        )
        .await;
        assert!(matches!(
            arm.query_vector("deploy").await,
            EmbedAttempt::Embedded { .. }
        ));
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(matches!(
            arm.query_vector("deploy").await,
            EmbedAttempt::Embedded { .. }
        ));
        assert_eq!(provider.embed_requests().await.len(), 2);
    }

    #[tokio::test]
    async fn different_queries_miss_the_cache() {
        let Harness { arm, provider } = harness(
            vec![
                Script::Embed(vec![Embedding {
                    vector: vec![1.0, 0.5]
                }]);
                2
            ],
            None,
        );
        assert!(matches!(
            arm.query_vector("deploy").await,
            EmbedAttempt::Embedded { .. }
        ));
        assert!(matches!(
            arm.query_vector("rollback").await,
            EmbedAttempt::Embedded { .. }
        ));
        assert_eq!(provider.embed_requests().await.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_vector_arms_warn_at_most_once_per_minute() {
        let (guard, lines) = testkit::logs::recorded();
        let Harness { arm, .. } = harness(vec![Script::EmbedHttpStatus(500); 3], None);
        let drop_once = || async {
            let attempt = arm.query_vector("deploy").await;
            let EmbedAttempt::Failed {
                failure,
                elapsed_ms,
            } = attempt
            else {
                panic!("the embed must fail for the drop to record")
            };
            arm.record_drop(&failure, elapsed_ms);
        };
        drop_once().await;
        drop_once().await;
        assert_eq!(arm.dropped(), 2);
        assert_eq!(lines.lock().len(), 1);
        assert!(
            lines.lock().first().is_some_and(|line| line
                .contains("[memory-search] vector arm dropped: provider request failed: http 500")),
            "the recorded warn line drifted: {:?}",
            lines.lock().first()
        );
        tokio::time::advance(WARN_THROTTLE).await;
        drop_once().await;
        assert_eq!(lines.lock().len(), 2);
        drop(guard);
    }

    #[tokio::test]
    async fn warn_only_failures_warn_without_counting() {
        let (guard, lines) = testkit::logs::recorded();
        let arm = harness(vec![Script::EmbedNetwork], None).arm;
        let EmbedAttempt::Failed { failure, .. } = arm.query_vector("query").await else {
            panic!("provider must fail")
        };
        arm.warn_drop(&failure, 3);
        assert_eq!(arm.dropped(), 0);
        assert_eq!(lines.lock().len(), 1);
        drop(guard);
    }

    #[test]
    fn query_input_carries_the_instruction_under_the_input_cap() {
        assert_eq!(
            compose_query_input("prefix ", "  spaced   out "),
            "prefix spaced out"
        );
        assert_eq!(compose_query_input("", "raw text"), "raw text");
        let long = compose_query_input(DEFAULT_EMBED_QUERY_INSTRUCTION, &"word ".repeat(4_000));
        assert_eq!(long.chars().count(), 8_000);
    }
}

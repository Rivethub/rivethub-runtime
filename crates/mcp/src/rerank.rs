mod snippets;

use std::sync::Arc;
use std::time::Duration;

use crate::render_search::{Clock, HitContext};
use config::{Config, recall::RerankSection};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use kernel::memory::{MemoryStore, SearchMode, SearchOutcome, SearchQuery, SearchRerank};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

const QUESTION: &str = "Which passage is most likely to contain the answer?";
const REPLY_LIMIT: usize = 65_536;

#[derive(Clone, Debug)]
pub(crate) struct Reranker {
    settings: RerankSection,
    client: reqwest::Client,
    endpoint: reqwest::Url,
}

#[derive(Serialize)]
struct Choice<'a> {
    kind: &'static str,
    state: String,
    question: &'static str,
    options: &'a [String],
}

#[derive(Deserialize)]
struct Decision {
    probabilities: Vec<f64>,
    #[serde(rename = "choice_index")]
    _choice_index: Option<usize>,
}

pub fn configured(config: &Config) -> Option<Arc<dyn SearchRerank>> {
    Reranker::from_config(config).map(|reranker| Arc::new(reranker) as Arc<dyn SearchRerank>)
}

impl Reranker {
    pub(crate) fn from_config(config: &Config) -> Option<Self> {
        let settings = config.memory.as_ref()?.recall.rerank.as_ref()?;
        match Self::build(settings) {
            Ok(reranker) => Some(reranker),
            Err(reason) => {
                skipped(reason);
                None
            }
        }
    }

    fn build(settings: &RerankSection) -> Result<Self, &'static str> {
        if let Some((_, reason)) = settings.problems().next() {
            return Err(reason);
        }
        let endpoint = settings.endpoint()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(settings.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| "client initialization failed")?;
        Ok(Self {
            settings: settings.clone(),
            endpoint,
            client,
        })
    }
}

#[async_trait::async_trait]
impl SearchRerank for Reranker {
    async fn apply(
        &self,
        store: &dyn MemoryStore,
        query: &SearchQuery,
        outcome: &mut SearchOutcome,
    ) {
        self.rank(store, query, outcome, None).await;
    }
}

impl Reranker {
    pub(crate) async fn rank(
        &self,
        store: &dyn MemoryStore,
        query: &SearchQuery,
        outcome: &mut SearchOutcome,
        context: Option<&HitContext>,
    ) {
        if query.mode != SearchMode::Hybrid || self.settings.top < 2 || outcome.results.len() < 2 {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(self.settings.timeout_ms);
        let classification = tokio::time::timeout_at(
            deadline,
            self.classify(store, query, outcome, context, deadline),
        )
        .await
        .map_err(|_| "timeout")
        .and_then(std::convert::identity);
        match classification {
            Ok(probabilities) if Instant::now() < deadline => reorder(outcome, probabilities),
            Err(reason) => skipped(reason),
            Ok(_) => skipped("timeout"),
        }
    }

    async fn classify(
        &self,
        store: &dyn MemoryStore,
        query: &SearchQuery,
        outcome: &SearchOutcome,
        context: Option<&HitContext>,
        deadline: Instant,
    ) -> Result<Vec<f64>, &'static str> {
        let count = self.settings.top.min(outcome.results.len());
        let head = outcome.results.get(..count).ok_or("missing hits")?;
        let loaded;
        let context = if let Some(context) = context {
            context
        } else {
            loaded = crate::memory::hit_context_for(store, outcome, head)
                .await
                .map_err(|_| "could not load hit context")?;
            &loaded
        };
        let clock = Clock {
            now: Timestamp::now(),
            zone: &TimeZone::UTC,
        };
        let mut options = Vec::with_capacity(count);
        for hit in head {
            let block = crate::render_search::hit_block(hit, outcome, context, true, clock)
                .map_err(|_| "could not render a hit")?;
            options.push(snippets::extract(
                &query.text,
                hit.title.as_deref(),
                &block,
                self.settings.chars,
                deadline,
            )?);
        }
        if Instant::now() >= deadline {
            return Err("timeout");
        }
        self.decide(&query.text, &options).await
    }

    async fn decide(&self, query: &str, options: &[String]) -> Result<Vec<f64>, &'static str> {
        let payload = Choice {
            kind: "choice",
            state: format!(
                "A person searching a memory of past working sessions asked this question:\n{query}"
            ),
            question: QUESTION,
            options,
        };
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&payload)
            .send()
            .await
            .map_err(|error| transport_reason(&error))?;
        if !response.status().is_success() {
            return Err("HTTP error");
        }
        let decision = read_decision(response).await?;
        validate(decision.probabilities, options.len())
    }
}

async fn read_decision(mut response: reqwest::Response) -> Result<Decision, &'static str> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_reason(&error))?
    {
        if body.len().saturating_add(chunk.len()) > REPLY_LIMIT {
            return Err("reply too large");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "malformed reply")
}

fn validate(probabilities: Vec<f64>, count: usize) -> Result<Vec<f64>, &'static str> {
    if probabilities.len() != count {
        return Err("probability count does not match hit count");
    }
    if probabilities.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return Err("invalid probabilities");
    }
    Ok(probabilities)
}

fn reorder(outcome: &mut SearchOutcome, probabilities: Vec<f64>) {
    let mut scored: Vec<_> = outcome
        .results
        .drain(..probabilities.len())
        .zip(probabilities)
        .collect();
    scored.sort_by(|(_, left), (_, right)| right.total_cmp(left));
    outcome
        .results
        .splice(..0, scored.into_iter().map(|(hit, _)| hit));
}

fn transport_reason(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "transport failed"
    }
}

fn skipped(reason: &'static str) {
    tracing::warn!(reason, "memory search rerank skipped");
}

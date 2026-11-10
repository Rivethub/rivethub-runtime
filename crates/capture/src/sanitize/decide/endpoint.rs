use std::collections::BTreeMap;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use futures::StreamExt as _;
use futures::TryStreamExt as _;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use super::chunks::Chunk;

const QUESTION: &str = "This line comes from an AI coding session that will be shared outside the author's household. Should it be redacted before sharing?";
const ANSWER_CAP: usize = 1 << 20;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const SHOWN_CHARS: usize = 200;
const SUM_TOLERANCE: f64 = 1e-3;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub concurrency: NonZeroUsize,
}

#[derive(Debug, Error)]
pub enum DecideError {
    #[error("the HTTP client for the decision model could not start: {0}")]
    Client(reqwest::Error),
    #[error("the decision model at {url} could not be reached: {source}")]
    Unreachable { url: String, source: reqwest::Error },
    #[error("the decision model at {url} answered HTTP {status}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("the decision model at {url} sent an answer over {ANSWER_CAP} bytes")]
    TooLarge { url: String },
    #[error(
        "the decision model at {url} sent an answer that is not two probabilities summing to 1: {detail}"
    )]
    Answer { url: String, detail: String },
    #[error("the decision model at {url} did not describe itself: {detail}")]
    Info { url: String, detail: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scores {
    pub model: String,
    pub calls: usize,
    pub scores: Vec<f64>,
}

#[derive(Serialize)]
struct Ask<'a> {
    kind: &'static str,
    state: &'a str,
    question: &'static str,
}

#[derive(Deserialize)]
struct Answer {
    probabilities: Vec<f64>,
}

#[derive(Deserialize)]
struct Info {
    model: String,
}

pub async fn score(endpoint: &Endpoint, chunks: &[Chunk]) -> Result<Scores, DecideError> {
    let client = reqwest::Client::builder()
        .timeout(CALL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(DecideError::Client)?;
    let base = endpoint.url.trim_end_matches('/');
    let model = info(&client, &format!("{base}/v1/decide/info")).await?;
    let mut unique: HashMap<&str, usize> = HashMap::new();
    let mut texts: Vec<&str> = Vec::new();
    let slots: Vec<usize> = chunks
        .iter()
        .map(|chunk| {
            *unique.entry(&chunk.text).or_insert_with(|| {
                texts.push(&chunk.text);
                texts.len() - 1
            })
        })
        .collect();
    let url = format!("{base}/v1/decide");
    let calls: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let (client, url, text) = (client.clone(), url.clone(), (*text).to_owned());
            async move { ask(&client, &url, &text).await.map(|score| (index, score)) }
        })
        .collect();
    let answered: BTreeMap<usize, f64> = futures::stream::iter(calls)
        .buffer_unordered(endpoint.concurrency.get())
        .try_collect()
        .await?;
    let scores = slots
        .into_iter()
        .map(|index| {
            answered
                .get(&index)
                .copied()
                .ok_or_else(|| DecideError::Answer {
                    url: url.clone(),
                    detail: format!("no answer for chunk text {index}"),
                })
        })
        .collect::<Result<_, _>>()?;
    Ok(Scores {
        model,
        calls: texts.len(),
        scores,
    })
}

async fn info(client: &reqwest::Client, url: &str) -> Result<String, DecideError> {
    let body = body(client.get(url), url).await?;
    let info: Info = serde_json::from_slice(&body).map_err(|error| DecideError::Info {
        url: url.to_owned(),
        detail: error.to_string(),
    })?;
    Ok(shown(&info.model))
}

async fn ask(client: &reqwest::Client, url: &str, state: &str) -> Result<f64, DecideError> {
    let request = client.post(url).json(&Ask {
        kind: "noul",
        state,
        question: QUESTION,
    });
    let body = body(request, url).await?;
    let answer = |detail: String| DecideError::Answer {
        url: url.to_owned(),
        detail,
    };
    let parsed: Answer =
        serde_json::from_slice(&body).map_err(|error| answer(error.to_string()))?;
    match parsed.probabilities.as_slice() {
        [no, yes]
            if [no, yes]
                .iter()
                .all(|p| p.is_finite() && (0.0..=1.0).contains(*p))
                && (no + yes - 1.0).abs() <= SUM_TOLERANCE =>
        {
            Ok(*yes)
        }
        other => Err(answer(format!("probabilities were {other:?}"))),
    }
}

async fn body(request: reqwest::RequestBuilder, url: &str) -> Result<Vec<u8>, DecideError> {
    let unreachable = |source| DecideError::Unreachable {
        url: url.to_owned(),
        source,
    };
    let mut response = request.send().await.map_err(unreachable)?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
        if body.len() + chunk.len() > ANSWER_CAP {
            return Err(DecideError::TooLarge {
                url: url.to_owned(),
            });
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(DecideError::Status {
            url: url.to_owned(),
            status: status.as_u16(),
            body: shown(&String::from_utf8_lossy(&body)),
        });
    }
    Ok(body)
}

fn shown(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(SHOWN_CHARS)
        .collect()
}

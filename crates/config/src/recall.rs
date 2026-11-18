use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecallSection {
    pub own_weight: Option<f64>,
    pub rerank: Option<RerankSection>,
}

impl RecallSection {
    #[must_use]
    pub fn weight(&self) -> f64 {
        self.own_weight.unwrap_or(1.25)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankSection {
    pub url: String,
    #[serde(default = "default_top")]
    pub top: usize,
    #[serde(default = "default_chars")]
    pub chars: usize,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl RerankSection {
    pub fn endpoint(&self) -> Result<url::Url, &'static str> {
        let invalid = "must be an HTTP or HTTPS URL without a query or fragment";
        let mut url = url::Url::parse(self.url.trim()).map_err(|_| invalid)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid);
        }
        let path = url.path().trim_end_matches('/');
        let endpoint = if path.ends_with("/v1/decide") {
            path.to_owned()
        } else {
            format!("{path}/v1/decide")
        };
        url.set_path(&endpoint);
        Ok(url)
    }

    pub fn problems(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        [
            self.endpoint()
                .err()
                .map(|message| ("memory.recall.rerank.url", message)),
            (!(1..=50).contains(&self.top))
                .then_some(("memory.recall.rerank.top", "must be between 1 and 50")),
            (!(1..=4000).contains(&self.chars))
                .then_some(("memory.recall.rerank.chars", "must be between 1 and 4000")),
            (!(1..=5000).contains(&self.timeout_ms)).then_some((
                "memory.recall.rerank.timeout_ms",
                "must be between 1 and 5000",
            )),
        ]
        .into_iter()
        .flatten()
    }
}

const fn default_top() -> usize {
    24
}

const fn default_chars() -> usize {
    300
}

const fn default_timeout_ms() -> u64 {
    1500
}

use std::path::Path;

use serde::Deserialize;

use crate::Error;
use crate::json::Json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canned {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    pub method: String,
    pub target: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl CapturedRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDelta {
    SamplingOnModelServer,
    OutputLimitOnModelServer,
    ServerDefaultToolChoice,
}

pub const B8_OMITTED_REQUEST_FIELDS: &[(&str, WireDelta)] = &[
    ("temperature", WireDelta::SamplingOnModelServer),
    ("top_p", WireDelta::SamplingOnModelServer),
    ("max_tokens", WireDelta::OutputLimitOnModelServer),
    ("tool_choice", WireDelta::ServerDefaultToolChoice),
];

pub fn strip_server_side_fields(body: &str) -> Result<String, Error> {
    let mut parsed: Json = serde_json::from_str(body)
        .map_err(|error| Error::format(Path::new("providers request body"), error.to_string()))?;
    let Json::Object(ref mut object) = parsed else {
        return Ok(body.to_owned());
    };
    for (field, _) in B8_OMITTED_REQUEST_FIELDS {
        object.remove(*field);
    }
    serde_json::to_string(&parsed)
        .map_err(|error| Error::format(Path::new("providers request body"), error.to_string()))
}

#[derive(Debug, Clone)]
pub struct ProviderTurn {
    pub requests: Vec<CapturedRequest>,
    pub chunks: Vec<Json>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RequestRecord {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub served: ServedRecord,
}

impl RequestRecord {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServedRecord {
    #[serde(default = "default_served_status")]
    pub status: u16,
    #[serde(rename = "bodyFile", default)]
    pub body_file: String,
}

fn default_served_status() -> u16 {
    200
}

#[derive(Debug, Clone)]
pub struct ProviderScenario {
    pub name: String,
    pub canned: Vec<Canned>,
    pub requests: Vec<RequestRecord>,
    pub chunks: Option<Vec<Json>>,
}

pub fn load_scenario(scenario: &Path) -> Result<ProviderScenario, Error> {
    let name = scenario
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| Error::format(scenario, "scenario requires a UTF-8 filename"))?
        .to_owned();
    let requests_path = scenario.join("requests.json");
    let text = crate::read_text(&requests_path)?;
    let requests: Vec<RequestRecord> = crate::parse_json(&requests_path, &text)?;
    let mut canned = Vec::new();
    for request in &requests {
        let body_path = scenario.join(&request.served.body_file);
        let body = crate::read_text(&body_path)?;
        canned.push(Canned {
            status: request.served.status,
            body,
        });
    }
    let chunks_path = scenario.join("chunks.json");
    let chunks = match std::fs::read_to_string(&chunks_path) {
        Ok(text) => Some(crate::parse_json(&chunks_path, &text)?),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(Error::io(&chunks_path, source)),
    };
    Ok(ProviderScenario {
        name,
        canned,
        requests,
        chunks,
    })
}

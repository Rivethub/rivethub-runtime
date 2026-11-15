use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;

use crate::Error;
use crate::Mismatch;
use crate::json::Masks;
use crate::json::json_masked_eq;
use crate::json::masked_eq;
use crate::json::substitute;

const MAX_HEADERS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub recipe: Option<String>,
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn bytes(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_string(), content_type.to_string())],
            body: body.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestHead {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

fn parse_head(head: &str) -> Option<RequestHead> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(head.as_bytes()) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    let method = request.method?.to_string();
    let target = request.path?.to_string();
    request.version?;
    let parsed = request
        .headers
        .iter()
        .map(|header| {
            (
                header.name.to_string(),
                String::from_utf8_lossy(header.value).trim().to_string(),
            )
        })
        .collect();
    Some(RequestHead {
        method,
        target,
        headers: parsed,
    })
}

fn status_of(status_line: &str) -> Result<u16, Mismatch> {
    let mut line = String::with_capacity(status_line.len() + 4);
    line.push_str(status_line);
    line.push_str("\r\n\r\n");
    let mut headers = [httparse::EMPTY_HEADER; 0];
    let mut response = httparse::Response::new(&mut headers);
    let parsed = response
        .parse(line.as_bytes())
        .map_err(|error| Mismatch::new("HTTP status", error.to_string()))?;
    if parsed.is_partial() {
        return Err(Mismatch::new("HTTP status", "incomplete status line"));
    }
    response
        .code
        .ok_or_else(|| Mismatch::new("HTTP status", "missing status code"))
}

type HeaderPair = (String, String);

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RequestRecord {
    #[serde(default)]
    pub via: Option<String>,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "bodyRecipe", default)]
    pub body_recipe: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponseRecord {
    #[serde(rename = "statusLine", default)]
    pub status_line: String,
    #[serde(default)]
    pub headers: Vec<HeaderPair>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "bodyBase64", default)]
    pub body_base64: Option<String>,
    #[serde(rename = "bodySha256", default)]
    pub body_sha256: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub runs: BTreeMap<String, RunRecord>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunRecord {
    #[serde(default)]
    pub cases: Vec<CaseRecord>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CaseRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub request: RequestRecord,
    #[serde(default)]
    pub response: ResponseRecord,
}

#[derive(Debug, Clone)]
pub struct HttpCase {
    pub id: String,
    pub request: RequestRecord,
    pub response: ResponseRecord,
}

pub fn cases(recording: &Path) -> Result<Vec<HttpCase>, Error> {
    let text = crate::read_text(recording)?;
    let doc: Recording = crate::parse_json(recording, &text)?;
    let mut cases = Vec::new();
    for (run, entry) in doc.runs {
        for case in entry.cases {
            let id = case.id.clone();
            cases.push(HttpCase {
                id: format!("{run}/{id}"),
                request: case.request,
                response: case.response,
            });
        }
    }
    Ok(cases)
}

pub fn replay(
    case: &HttpCase,
    masks: &Masks,
    handler: impl FnOnce(&Request) -> Response,
) -> Result<(), Mismatch> {
    let Some(request) = build_request(&case.request, masks) else {
        return Err(Mismatch::new(&case.id, "recording request unreadable"));
    };
    let actual = handler(&request);
    compare_response(&case.id, &request.method, &case.response, &actual, masks)
}

fn build_request(recorded: &RequestRecord, masks: &Masks) -> Option<Request> {
    let head = parse_head(&substitute(&recorded.head, masks))?;
    let body = recorded
        .body
        .as_deref()
        .map_or_else(Vec::new, |text| substitute(text, masks).into_bytes());
    let recipe = recorded
        .body_recipe
        .as_deref()
        .map(|text| substitute(text, masks));
    Some(Request {
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
        recipe,
        via: recorded.via.clone(),
    })
}

fn is_transport_header(lower: &str) -> bool {
    matches!(
        lower,
        "date" | "connection" | "keep-alive" | "transfer-encoding" | "content-length"
    )
}

fn compare_headers(
    id: &str,
    recorded: &[HeaderPair],
    actual: &[(String, String)],
    masks: &Masks,
) -> Result<(), Mismatch> {
    let mut used = vec![false; actual.len()];
    for (name, value) in recorded {
        let matched = actual
            .iter()
            .zip(&mut used)
            .find(|((actual_name, _), taken)| !**taken && actual_name.eq_ignore_ascii_case(name))
            .map(|((_, actual_value), taken)| {
                *taken = true;
                masked_eq(value, actual_value, masks)
            })
            .transpose()?;
        match matched {
            Some(true) => {}
            Some(false) => {
                return Err(Mismatch::new(id, format!("header {name}: value mismatch")));
            }
            None => {
                if !is_transport_header(&name.to_ascii_lowercase()) {
                    return Err(Mismatch::new(id, format!("header {name} missing")));
                }
            }
        }
    }
    Ok(())
}

fn recorded_body(recorded: &ResponseRecord) -> Result<Vec<u8>, Mismatch> {
    match (&recorded.body, &recorded.body_base64) {
        (Some(text), None) => Ok(text.as_bytes().to_vec()),
        (None, Some(encoded)) => BASE64
            .decode(encoded)
            .map_err(|error| Mismatch::new("HTTP body", error.to_string())),
        (None, None) => Ok(Vec::new()),
        (Some(_), Some(_)) => Err(Mismatch::new("HTTP body", "ambiguous body encoding")),
    }
}

pub(crate) fn compare_text(
    id: &str,
    want_text: &str,
    act_text: &str,
    masks: &Masks,
) -> Result<(), Mismatch> {
    if masked_eq(want_text, act_text, masks)? {
        return Ok(());
    }
    let want_json = serde_json::from_str(want_text)
        .map_err(|error| Mismatch::new(id, format!("body mismatch: {error}")))?;
    let act_json = serde_json::from_str(act_text)
        .map_err(|error| Mismatch::new(id, format!("body mismatch: {error}")))?;
    let equal = json_masked_eq(&want_json, &act_json, masks)?;
    if equal {
        Ok(())
    } else {
        Err(Mismatch::new(
            id,
            format!("body mismatch\n    recorded: {want_text}\n    actual:   {act_text}"),
        ))
    }
}

pub(crate) fn compare_response(
    id: &str,
    method: &str,
    recorded: &ResponseRecord,
    actual: &Response,
    masks: &Masks,
) -> Result<(), Mismatch> {
    if recorded.status_line.is_empty() {
        return Err(Mismatch::new(id, "recording has no response status"));
    }
    let want = status_of(&recorded.status_line)?;
    if want != actual.status {
        return Err(Mismatch::new(
            id,
            format!("status {} != recorded {want}", actual.status),
        ));
    }
    compare_headers(id, &recorded.headers, &actual.headers, masks)?;
    let want_body = recorded_body(recorded)?;
    if method == "HEAD" || actual.status < 200 || matches!(actual.status, 204 | 205 | 304) {
        if !want_body.is_empty() || !actual.body.is_empty() {
            return Err(Mismatch::new(id, "response must not contain a body"));
        }
        return Ok(());
    }
    if recorded.body_base64.is_some() {
        if want_body != actual.body {
            return Err(Mismatch::new(id, "binary body mismatch"));
        }
        return Ok(());
    }
    if let Some(want_sha) = &recorded.body_sha256 {
        let digest = Sha256::digest(&actual.body);
        if format!("{digest:x}") != *want_sha {
            return Err(Mismatch::new(id, "body sha256 mismatch"));
        }
    }
    let want_text =
        std::str::from_utf8(&want_body).map_err(|error| Mismatch::new(id, error.to_string()))?;
    let act_text =
        std::str::from_utf8(&actual.body).map_err(|error| Mismatch::new(id, error.to_string()))?;
    if recorded.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-type") && value.starts_with("application/json")
    }) {
        let want_json = serde_json::from_str(want_text)
            .map_err(|error| Mismatch::new(id, error.to_string()))?;
        let act_json =
            serde_json::from_str(act_text).map_err(|error| Mismatch::new(id, error.to_string()))?;
        if !json_masked_eq(&want_json, &act_json, masks)? {
            return Err(Mismatch::new(
                id,
                format!("body mismatch\n    recorded: {want_text}\n    actual:   {act_text}"),
            ));
        }
        return Ok(());
    }
    compare_text(id, want_text, act_text, masks)
}

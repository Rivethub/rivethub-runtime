use axum::extract::FromRequest;
use axum::extract::Request;
use serde::Serialize;
use wire::capture::CaptureBatchRequest;

use crate::error::ApiError;
use crate::error::invalid_json;
use crate::extract::limited_bytes;

const CAPTURE_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct ValidatedBatch(pub(crate) CaptureBatchRequest);

impl<S> FromRequest<S> for ValidatedBatch
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = limited_bytes(request, state).await?;
        let sanitized = wire::json::replace_lone_surrogates(&bytes);
        let batch: CaptureBatchRequest =
            serde_json::from_slice(&sanitized).map_err(|_| invalid_json())?;
        validate(batch)
    }
}

pub(crate) fn validate(batch: CaptureBatchRequest) -> Result<ValidatedBatch, ApiError> {
    let mut issues = Vec::new();
    non_empty(&mut issues, &batch.session_key, "session_key");
    non_empty(&mut issues, &batch.agent, "agent");
    for (index, message) in batch.messages.iter().enumerate() {
        if message.event_id.is_empty() {
            issues.push(too_small(message_path(index, "event_id")));
        }
    }
    if issues.is_empty() {
        Ok(ValidatedBatch(batch))
    } else {
        Err(ApiError::BadRequest(issue_text(&issues)?))
    }
}

#[derive(Serialize)]
struct Issue {
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<&'static str>,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inclusive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<&'static str>,
    path: Vec<PathSegment>,
    message: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
enum PathSegment {
    Field(String),
    Index(usize),
}

fn too_small(path: Vec<PathSegment>) -> Issue {
    Issue {
        origin: Some("string"),
        code: "too_small",
        minimum: Some(1),
        inclusive: Some(true),
        format: None,
        pattern: None,
        path,
        message: "Too small: expected string to have >=1 characters",
    }
}

fn trimmed(path: Vec<PathSegment>) -> Issue {
    Issue {
        origin: None,
        code: "custom",
        minimum: None,
        inclusive: None,
        format: None,
        pattern: None,
        path,
        message: "Invalid input",
    }
}

fn issue_text(issues: &[Issue]) -> Result<String, ApiError> {
    serde_json::to_string_pretty(issues).map_err(ApiError::Serialize)
}

#[must_use]
pub fn body_limit() -> usize {
    CAPTURE_BODY_LIMIT
}

fn non_empty(issues: &mut Vec<Issue>, value: &str, field: &str) {
    let path = vec![PathSegment::Field(field.to_string())];
    if value.is_empty() {
        issues.push(too_small(path.clone()));
    }
    if value.trim().is_empty() {
        issues.push(trimmed(path));
    }
}

fn message_path(index: usize, field: &str) -> Vec<PathSegment> {
    vec![
        PathSegment::Field("messages".to_string()),
        PathSegment::Index(index),
        PathSegment::Field(field.to_string()),
    ]
}

#[cfg(test)]
mod validated_batch_tests {
    use wire::capture::CaptureBatchRequest;
    use wire::capture::CaptureMessage;
    use wire::message::Role;

    use super::validate;

    #[test]
    fn validated_batches_carry_typed_timestamps() {
        let batch = CaptureBatchRequest {
            session_key: "s".to_owned(),
            agent: "a".to_owned(),
            channel: None,
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: "e1".to_owned(),
                role: Role::User,
                content: "hi".to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: Some(
                    wire::time::parse_rfc3339("2026-10-10T12:00:00+02:00").expect("offset instant"),
                ),
            }],
        };
        let validated = validate(batch).expect("valid batch");
        let entries = validated.0.entries();
        let created_at = entries.first().and_then(|entry| entry.created_at);
        assert_eq!(
            created_at,
            Some(wire::time::parse_rfc3339("2026-10-10T10:00:00Z").expect("instant"))
        );
    }
}

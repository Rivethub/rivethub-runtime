use std::sync::Arc;

use axum::body::to_bytes;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use gateway::error::ApiError;
use kernel::memory::MemoryStoreError;
use parking_lot::Mutex;
use wire::gateway::GatewayError;

const PROBE: &str = "Matching variant not found";

#[derive(Debug)]
struct Probe;

impl std::fmt::Display for Probe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(PROBE)
    }
}

impl std::error::Error for Probe {}

struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

struct LineVisitor<'a>(&'a mut String);

impl tracing::field::Visit for LineVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&rendered);
        self.0.push(';');
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(value);
        self.0.push(';');
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_non_zero_u64(std::num::NonZeroU64::MIN)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        let mut line = String::new();
        event.record(&mut LineVisitor(&mut line));
        self.lines.lock().push(line);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

fn install() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let guard = crate::common::capture_logs(Capture {
        lines: Arc::clone(&lines),
    });
    (guard, lines)
}

fn query_failure() -> ApiError {
    ApiError::Store(MemoryStoreError::Query {
        message: "invalid stored summary kind".to_owned(),
        source: Box::new(Probe),
    })
}

async fn body_text(
    response: axum::response::Response,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    let parsed: GatewayError = serde_json::from_str(&text)?;
    assert_eq!(text, format!("{{\"error\":\"{}\"}}", parsed.error));
    Ok((status, text))
}

#[tokio::test]
async fn a_store_query_failure_is_logged_once_per_response()
-> Result<(), Box<dyn std::error::Error>> {
    let (_guard, lines) = install();
    let first = query_failure().into_response();
    let second = query_failure().into_response();
    let recorded = lines.lock().clone();
    assert_eq!(recorded.len(), 2);
    for line in &recorded {
        assert_eq!(line.matches(PROBE).count(), 1, "{line}");
        assert!(line.contains("memory query failed"), "{line}");
    }
    for response in [first, second] {
        let (status, text) = body_text(response).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(text, "{\"error\":\"memory query failed\"}");
        assert!(!text.contains(PROBE));
    }
    Ok(())
}

#[tokio::test]
async fn an_unavailable_store_stays_a_bare_unavailable_response()
-> Result<(), Box<dyn std::error::Error>> {
    let (_guard, lines) = install();
    let response = ApiError::Store(MemoryStoreError::Unavailable(Box::new(Probe))).into_response();
    assert!(lines.lock().is_empty());
    let (status, text) = body_text(response).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(text, "{\"error\":\"memory store unavailable\"}");
    assert!(!text.contains(PROBE));
    Ok(())
}

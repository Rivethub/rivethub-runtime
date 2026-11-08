use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use wire::model_call::{
    CallOutcome, CallUsage, ModelCallBatchRequest, ModelCallBatchResult, SessionModelCalls,
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/model_call/batch.json"
);

fn fixture(key: &str) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(FIXTURE).map_err(|error| format!("{FIXTURE}: {error}"))?;
    let all: Value = serde_json::from_str(&text).map_err(|error| format!("{FIXTURE}: {error}"))?;
    all.get(key)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("{key}: not a list of cases"))
}

fn roundtrips<T: DeserializeOwned + Serialize>(key: &str) -> Result<(), String> {
    for (index, case) in fixture(key)?.into_iter().enumerate() {
        let typed: T = serde_json::from_value(case.clone())
            .map_err(|error| format!("{key}[{index}]: {error}"))?;
        let written =
            serde_json::to_value(&typed).map_err(|error| format!("{key}[{index}]: {error}"))?;
        if written != case {
            return Err(format!(
                "{key}[{index}] changed in a roundtrip\n  in : {case}\n  out: {written}"
            ));
        }
    }
    Ok(())
}

#[test]
fn batch_requests_roundtrip() -> Result<(), String> {
    roundtrips::<ModelCallBatchRequest>("requests")
}

#[test]
fn batch_results_roundtrip() -> Result<(), String> {
    roundtrips::<ModelCallBatchResult>("results")
}

#[test]
fn session_reads_roundtrip() -> Result<(), String> {
    roundtrips::<SessionModelCalls>("sessions")
}

#[test]
fn a_call_needs_only_its_id_start_provider_and_outcome() -> Result<(), serde_json::Error> {
    let request: ModelCallBatchRequest = serde_json::from_str(
        r#"{"calls":[{"call_id":"c","started_at":"2026-11-08T12:00:00Z","provider":"p","outcome":"server_error"}]}"#,
    )?;
    let call = &request.calls[0];
    assert_eq!(call.outcome, CallOutcome::ServerError);
    assert!(call.headers.is_empty());
    assert_eq!(call.usage, None);
    Ok(())
}

#[test]
fn usage_counts_default_to_zero() -> Result<(), serde_json::Error> {
    let usage: CallUsage = serde_json::from_str(r#"{"output":7}"#)?;
    assert_eq!(
        usage,
        CallUsage {
            output: 7,
            ..CallUsage::default()
        }
    );
    Ok(())
}

#[test]
fn an_unknown_outcome_is_refused() {
    assert!(serde_json::from_str::<CallOutcome>(r#""exploded""#).is_err());
}

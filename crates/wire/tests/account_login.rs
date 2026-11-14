use serde::{Deserialize, Serialize};
use wire::gateway::account::{SignInProgress, SignInRequest, SignInStarted};

#[derive(Debug, Serialize, Deserialize)]
struct Golden {
    start: SignInRequest,
    started: SignInStarted,
    pending: SignInProgress,
    expired: SignInProgress,
    denied: SignInProgress,
}

#[test]
fn device_login_golden_round_trips_without_secrets() -> Result<(), Box<dyn std::error::Error>> {
    let text = include_str!("golden/gateway/account-device-login.json");
    let parsed: Golden = serde_json::from_str(text)?;
    let rendered = serde_json::to_string(&parsed)?;
    assert_eq!(rendered, text.trim());
    assert!(!rendered.contains("deviceCode"));
    assert!(!rendered.contains("token"));
    Ok(())
}

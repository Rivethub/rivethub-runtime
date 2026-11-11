use chrono::{DateTime, Utc};
use kernel::model_calls::{
    CallTotals, CredentialHeader, ModelCall, ProxyHeaders, TurnMetadata, correlate,
};
use values::{CallOutcome, CallUsage, HarnessId};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CLAUDE_SESSION: &str = "00000000-0000-4000-8000-0000000000c1";
const CODEX_SESSION: &str = "00000000-0000-7000-8000-0000000000c2";

fn headers(pairs: &[(&str, &str)]) -> Result<ProxyHeaders, CredentialHeader> {
    ProxyHeaders::parse(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    )
}

fn claude_headers() -> Result<ProxyHeaders, CredentialHeader> {
    headers(&[
        ("X-Claude-Code-Session-Id", CLAUDE_SESSION),
        ("User-Agent", "claude-cli/2.1.319 (external, cli)"),
        ("anthropic-version", "2023-06-01"),
    ])
}

fn codex_headers(metadata: &str) -> Result<ProxyHeaders, CredentialHeader> {
    headers(&[
        ("session-id", CODEX_SESSION),
        ("thread-id", CODEX_SESSION),
        ("originator", "codex_exec"),
        ("user-agent", "codex_exec/0.167.0 (Linux; x86_64)"),
        ("x-codex-turn-metadata", metadata),
    ])
}

fn call(outcome: CallOutcome, model: Option<&str>, usage: Option<CallUsage>) -> ModelCall {
    ModelCall {
        call_id: "call".to_owned(),
        started_at: DateTime::<Utc>::UNIX_EPOCH,
        finished_at: None,
        provider: "anthropic".to_owned(),
        model_requested: None,
        model_served: model.map(str::to_owned),
        account: None,
        status_code: None,
        outcome,
        retry_after_secs: None,
        usage,
        session: None,
        turn: None,
    }
}

#[test]
fn a_claude_code_request_names_its_session_key() -> TestResult {
    let found = correlate(&claude_headers()?).ok_or("no correlation")?;
    assert_eq!(found.session.harness(), HarnessId::ClaudeCode);
    assert_eq!(found.session.native(), CLAUDE_SESSION);
    assert_eq!(
        found.session.as_str(),
        format!("claude-code:{CLAUDE_SESSION}")
    );
    assert_eq!(found.turn, None);
    Ok(())
}

#[test]
fn a_codex_request_names_its_session_key_and_turn() -> TestResult {
    let metadata = format!(
        r#"{{"session_id":"{CODEX_SESSION}","turn_id":"00000000-0000-7000-8000-0000000000d1","request_kind":"turn","sandbox":"none"}}"#
    );
    let found = correlate(&codex_headers(&metadata)?).ok_or("no correlation")?;
    assert_eq!(found.session.as_str(), format!("codex:{CODEX_SESSION}"));
    assert_eq!(
        found.turn,
        Some(TurnMetadata {
            turn_id: Some("00000000-0000-7000-8000-0000000000d1".to_owned()),
            request_kind: Some("turn".to_owned()),
        })
    );
    Ok(())
}

#[test]
fn malformed_codex_turn_metadata_is_ignored_and_the_session_still_matches() -> TestResult {
    for metadata in ["not json", "[]", r#"{"turn_id":7}"#, "{}"] {
        let found = correlate(&codex_headers(metadata)?).ok_or("no correlation")?;
        assert_eq!(found.session.as_str(), format!("codex:{CODEX_SESSION}"));
        assert_eq!(found.turn, None, "{metadata}");
    }
    Ok(())
}

#[test]
fn an_unknown_client_or_a_missing_session_header_correlates_to_nothing() -> TestResult {
    let unknown = headers(&[
        ("user-agent", "curl/8.0"),
        ("session-id", CODEX_SESSION),
        ("x-claude-code-session-id", CLAUDE_SESSION),
    ])?;
    assert_eq!(correlate(&unknown), None);
    let no_agent = headers(&[("x-claude-code-session-id", CLAUDE_SESSION)])?;
    assert_eq!(correlate(&no_agent), None);
    let no_session = headers(&[("user-agent", "claude-cli/2.1.319")])?;
    assert_eq!(correlate(&no_session), None);
    let blank = headers(&[
        ("user-agent", "claude-cli/2.1.319"),
        ("x-claude-code-session-id", "  "),
    ])?;
    assert_eq!(correlate(&blank), None);
    let crossed = headers(&[
        ("user-agent", "claude-cli/2.1.319"),
        ("session-id", CODEX_SESSION),
    ])?;
    assert_eq!(correlate(&crossed), None);
    Ok(())
}

#[test]
fn credential_headers_are_refused_by_name_in_any_case() {
    for name in [
        "Authorization",
        "proxy-authorization",
        "X-API-Key",
        "Cookie",
        "ChatGPT-Account-Id",
        "x-session-token",
        "X-Client-Secret",
        "x-refresh-TOKEN-id",
    ] {
        let refused = headers(&[("user-agent", "claude-cli/2.1.319"), (name, "value")]);
        assert_eq!(
            refused,
            Err(CredentialHeader {
                name: name.to_ascii_lowercase()
            })
        );
    }
}

#[test]
fn header_names_are_matched_case_insensitively() -> TestResult {
    let parsed = headers(&[("USER-AGENT", "claude-cli/2.1.319"), ("X-A", "1")])?;
    assert_eq!(parsed.get("user-agent"), Some("claude-cli/2.1.319"));
    assert_eq!(parsed.get("x-a"), Some("1"));
    assert_eq!(parsed.get("X-A"), None);
    Ok(())
}

#[test]
fn totals_sum_usage_count_errors_and_group_by_served_model() {
    let usage = |input, output, cache_read, cache_write, reasoning| {
        Some(CallUsage {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        })
    };
    let mut requested_only = call(CallOutcome::Completed, None, usage(1, 1, 0, 0, 0));
    requested_only.model_requested = Some("alias".to_owned());
    let calls = [
        call(
            CallOutcome::Completed,
            Some("model-a"),
            usage(10, 20, 30, 40, 5),
        ),
        call(
            CallOutcome::Completed,
            Some("model-a"),
            usage(1, 2, 3, 4, 5),
        ),
        call(CallOutcome::RateLimited, Some("model-b"), None),
        call(CallOutcome::Aborted, None, None),
        requested_only,
    ];
    let totals = CallTotals::of(&calls);
    assert_eq!(totals.calls, 5);
    assert_eq!(totals.errors, 2);
    assert_eq!(
        totals.usage,
        CallUsage {
            input: 12,
            output: 23,
            cache_read: 33,
            cache_write: 44,
            reasoning: 10,
        }
    );
    let models: Vec<(&str, u64)> = totals
        .models
        .iter()
        .map(|(model, count)| (model.as_str(), *count))
        .collect();
    assert_eq!(
        models,
        [("alias", 1), ("model-a", 2), ("model-b", 1), ("unknown", 1)]
    );
}

#[test]
fn totals_of_no_calls_are_zero() {
    assert_eq!(CallTotals::of(&[]), CallTotals::default());
}

use crate::common;

use common::answer;
use futures::executor::block_on;
use gateway::auth::Decision;
use gateway::auth::OriginCheck;
use gateway::auth::OriginPolicy;
use testkit::Request;
use wire::gateway::GatewayError;

fn policy() -> OriginPolicy {
    OriginPolicy::new(
        vec!["https://hub.example.test".to_string()],
        vec!["gateway.example.test".to_string()],
    )
}

fn decide(origin: Option<&str>, host: Option<&str>, loopback: bool) -> Decision {
    policy().evaluate(&OriginCheck {
        origin,
        host,
        caller_loopback: loopback,
        plain_http: true,
    })
}

#[test]
fn loopback_addresses_are_exactly_the_recorded_set() {
    use gateway::auth::is_loopback_remote;
    use std::net::SocketAddr;
    let loopback = |text: &str| is_loopback_remote(text.parse::<SocketAddr>().expect("addr"));
    assert!(loopback("127.0.0.1:8080"));
    assert!(loopback("[::1]:8080"));
    assert!(loopback("[::ffff:127.0.0.1]:8080"));
    assert!(!loopback("127.0.0.2:8080"));
    assert!(!loopback("192.0.2.7:8080"));
    assert!(!loopback("[2001:db8::1]:8080"));
}

#[test]
fn origin_policy_rejects_null_and_unparseable_origins() {
    assert_eq!(
        decide(None, Some("127.0.0.1:5174"), true),
        Decision::Allow { echo: None }
    );
    assert_eq!(
        decide(Some("null"), Some("127.0.0.1:5174"), true),
        Decision::Deny {
            reason: "origin \"null\" is not allowed".to_string()
        }
    );
    assert_eq!(
        decide(Some("not a url"), Some("127.0.0.1:5174"), true),
        Decision::Deny {
            reason: "origin \"not a url\" is not allowed".to_string()
        }
    );
}

#[test]
fn origin_policy_allows_same_origin_and_builtin() {
    assert_eq!(
        decide(Some("http://127.0.0.1:5174"), Some("127.0.0.1:5174"), true),
        Decision::Allow {
            echo: Some("http://127.0.0.1:5174".to_string())
        }
    );
    assert_eq!(
        decide(Some("https://127.0.0.1:5174"), Some("127.0.0.1:5174"), true),
        Decision::Allow {
            echo: Some("https://127.0.0.1:5174".to_string())
        }
    );
    assert_eq!(
        decide(
            Some("http://127.0.0.1:5174/some/page"),
            Some("127.0.0.1:5174"),
            true
        ),
        Decision::Allow {
            echo: Some("http://127.0.0.1:5174".to_string())
        }
    );
    assert_eq!(
        decide(Some("HTTP://127.0.0.1:5174"), Some("127.0.0.1:5174"), true),
        Decision::Allow {
            echo: Some("http://127.0.0.1:5174".to_string())
        }
    );
    assert_eq!(
        decide(Some("http://localhost:80"), Some("localhost"), true),
        Decision::Allow {
            echo: Some("http://localhost".to_string())
        }
    );
    assert_eq!(
        decide(Some("app://bundle"), Some("127.0.0.1:5174"), true),
        Decision::Allow {
            echo: Some("app://bundle".to_string())
        }
    );
    assert_eq!(
        decide(Some("APP://BUNDLE"), Some("127.0.0.1:5174"), true),
        Decision::Allow {
            echo: Some("app://bundle".to_string())
        }
    );
    assert_eq!(
        decide(
            Some("https://hub.example.test:443"),
            Some("127.0.0.1:5174"),
            true
        ),
        Decision::Allow {
            echo: Some("https://hub.example.test".to_string())
        }
    );
}

#[test]
fn origin_policy_applies_the_allow_lists() {
    assert_eq!(
        decide(Some("http://127.0.0.1"), Some("127.0.0.1:5174"), true),
        Decision::Deny {
            reason: "origin \"http://127.0.0.1\" is not allowed (add it to the gateway allowed_origins setting)"
                .to_string()
        }
    );
    assert_eq!(
        decide(
            Some("https://hub.example.test"),
            Some("127.0.0.1:5174"),
            true
        ),
        Decision::Allow {
            echo: Some("https://hub.example.test".to_string())
        }
    );
    assert_eq!(
        decide(Some("https://evil.example"), Some("127.0.0.1:5174"), true),
        Decision::Deny {
            reason:
                "origin \"https://evil.example\" is not allowed (add it to the gateway allowed_origins setting)"
                    .to_string()
        }
    );
}

#[test]
fn plain_http_host_rule_applies_to_loopback_callers_only() {
    assert_eq!(
        decide(None, Some("evil.example"), true),
        Decision::Deny {
            reason:
                "host \"evil.example\" is not allowed (add it to the gateway allowed_hosts setting)"
                    .to_string()
        }
    );
    assert_eq!(
        decide(None, Some("evil.example:5174"), true),
        Decision::Deny {
            reason:
                "host \"evil.example\" is not allowed (add it to the gateway allowed_hosts setting)"
                    .to_string()
        }
    );
    assert_eq!(
        decide(None, Some(""), true),
        Decision::Deny {
            reason: "host \"\" is not allowed (add it to the gateway allowed_hosts setting)"
                .to_string()
        }
    );
    assert_eq!(
        decide(None, None, true),
        Decision::Deny {
            reason: "host \"\" is not allowed (add it to the gateway allowed_hosts setting)"
                .to_string()
        }
    );
    for host in [
        "gateway.example.test:5174",
        "hub.localhost:5174",
        "LOCALHOST:5174",
        "[::1]:5174",
        "127.0.0.1:5174",
    ] {
        assert_eq!(
            decide(None, Some(host), true),
            Decision::Allow { echo: None },
            "{host}"
        );
    }
    assert_eq!(
        decide(None, Some("evil.example"), false),
        Decision::Allow { echo: None }
    );
    assert_eq!(
        decide(None, Some(""), false),
        Decision::Allow { echo: None }
    );
}

fn response(
    method: &str,
    target: &str,
    via: Option<&str>,
    headers: &[(&str, &str)],
) -> testkit::Response {
    let mut header_pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    header_pairs.push(("Host".to_string(), "127.0.0.1:5174".to_string()));
    let (gateway, _) = common::gateway();
    let request = Request {
        method: method.to_string(),
        target: target.to_string(),
        headers: header_pairs,
        body: Vec::new(),
        recipe: None,
        via: via.map(str::to_string),
    };
    match block_on(common::answer(gateway.router(false), request)) {
        Ok(response) => {
            assert_eq!(
                response
                    .headers
                    .iter()
                    .any(|(name, _)| name == "access-control-allow-methods"),
                response.status != 403,
                "CORS pair present except on origin denials"
            );
            response
        }
        Err(error) => testkit::Response {
            status: 599,
            headers: Vec::new(),
            body: error.into_bytes(),
        },
    }
}

#[test]
fn remote_callers_get_unauthorized_json_on_api_paths() {
    let answer = response("GET", "/api/memory/stats", Some("remote"), &[]);
    assert_eq!(answer.status, 401);
    let parsed: GatewayError = serde_json::from_slice(&answer.body).expect("error body");
    assert_eq!(parsed.error, "unauthorized");

    let html = response(
        "GET",
        "/api/memory/stats",
        Some("remote"),
        &[("Accept", "text/html")],
    );
    assert_eq!(html.status, 401);
    let parsed: GatewayError = serde_json::from_slice(&html.body).expect("error body");
    assert_eq!(parsed.error, "unauthorized");
}

#[test]
fn remote_browsers_get_the_html_gate_off_api_paths() -> Result<(), Box<dyn std::error::Error>> {
    let (gateway, _) = common::gateway();
    let request = Request {
        method: "GET".to_string(),
        target: "/".to_string(),
        headers: vec![
            ("Host".to_string(), "127.0.0.1:5174".to_string()),
            (
                "Accept".to_string(),
                "text/html,application/xhtml+xml,application/xml;q=0.9".to_string(),
            ),
        ],
        body: Vec::new(),
        recipe: None,
        via: Some("remote".to_string()),
    };
    let response = block_on(answer(gateway.router(false), request)).expect("in-process answer");
    assert_eq!(response.status, 401);
    let content_type = response
        .headers
        .iter()
        .find(|(name, _)| name == "content-type")
        .map(|(_, value)| value.as_str())
        .ok_or("the html gate carries no content-type")?;
    assert_eq!(content_type, "text/html; charset=utf-8");
    let text = String::from_utf8_lossy(&response.body);
    assert!(text.contains("Device certificate required"));
    assert!(text.contains("RivetHub desktop"));
    assert!(text.contains("<code>rh cert renew</code>"));
    assert!(text.contains("/healthz"));
    Ok(())
}

fn documented_origin_example(doc: &str) -> Option<&str> {
    doc.split("```yaml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
}

fn loopback_check(origin: &str) -> OriginCheck<'_> {
    OriginCheck {
        origin: Some(origin),
        host: Some("127.0.0.1:5174"),
        caller_loopback: true,
        plain_http: true,
    }
}

#[test]
fn the_documented_vite_dev_origin_example_admits_exactly_that_origin() {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/CLI.md"))
        .expect("CLI doc");
    let example = documented_origin_example(&doc).expect("documented allowed_origins example");
    let config: config::Config = serde_yaml_ng::from_str(example).expect("documented config");
    let home = tempfile::tempdir().expect("home");
    let env = config::Environment::from_vars([("HOME", home.path().to_str().expect("utf-8 home"))]);
    let settings = gateway::Settings::from_config_in(
        &config,
        &env,
        "gold-host".to_string(),
        "golden-node".to_string(),
    )
    .expect("settings");
    let policy = OriginPolicy::new(settings.allowed_origins, settings.allowed_hosts);
    for origin in ["http://127.0.0.1:5273", "http://localhost:5273"] {
        assert_eq!(
            policy.evaluate(&loopback_check(origin)),
            Decision::Allow {
                echo: Some(origin.to_string())
            }
        );
    }
    assert_eq!(
        policy.evaluate(&loopback_check("http://127.0.0.1:5274")),
        Decision::Deny {
            reason: "origin \"http://127.0.0.1:5274\" is not allowed (add it to the gateway allowed_origins setting)"
                .to_string()
        }
    );
}

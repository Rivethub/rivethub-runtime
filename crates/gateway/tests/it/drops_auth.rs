use std::net::{IpAddr, Ipv4Addr};

use reqwest::{Method, StatusCode};

use wire::gateway::GatewayError;
use wire::gateway::drops::Uploaded;
use wire::gateway::tenancy::MeResponse;

use crate::drops::{Answer, Failure, Node, Outcome, Payload, Shape, fresh_id, sealed, send};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

async fn me(node: &Node, headers: &[(&str, &str)]) -> Result<Answer, Failure> {
    send(
        &node.client(LOOPBACK)?,
        Method::GET,
        &format!("{}/api/me", node.base),
        headers,
        Payload::Empty,
    )
    .await
}

fn user(answer: &Answer) -> Result<String, Failure> {
    Ok(serde_json::from_slice::<MeResponse>(&answer.body)?.user)
}

const NO_CREDENTIALS: &str = r#"Bearer realm="rivethub""#;
const INVALID_TOKEN: &str = r#"Bearer realm="rivethub", error="invalid_token""#;

fn personal() -> Shape {
    Shape {
        enabled: false,
        ..Shape::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_header_less_loopback_caller_needs_a_token_too() -> Outcome {
    for shape in [Shape::default(), personal()] {
        let node = Node::start(shape).await?;
        for (route, answer) in [
            ("/api/me", me(&node, &[]).await?),
            ("/api/memory/stats", stats(&node, &[]).await?),
        ] {
            assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{route}");
            assert_eq!(
                answer.header("www-authenticate"),
                Some(NO_CREDENTIALS),
                "{route}"
            );
        }
        let via = me(&node, &[("via", "1.1 forwarder")]).await?;
        assert_eq!(
            via.status,
            StatusCode::UNAUTHORIZED,
            "a forwarder that only adds Via"
        );
        let token = [("authorization", "Bearer alice-token")];
        let alice = me(&node, &token).await?;
        assert_eq!(alice.status, StatusCode::OK);
        assert_eq!(user(&alice)?, "alice");
        assert_eq!(stats(&node, &token).await?.status, StatusCode::OK);
        let wrong = stats(&node, &[("authorization", "Bearer rhca_unknown")]).await?;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
        assert_eq!(wrong.header("www-authenticate"), Some(INVALID_TOKEN));
        node.stop().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn every_401_carries_a_bearer_challenge() -> Outcome {
    let node = Node::start(personal()).await?;
    let remote = node.client(crate::drops::REMOTE)?;
    let ask = |path: &'static str, headers: &'static [(&'static str, &'static str)]| {
        let url = format!("{}{path}", node.base);
        let remote = remote.clone();
        async move { send(&remote, Method::GET, &url, headers, Payload::Empty).await }
    };
    for (label, answer, challenge) in [
        (
            "guard, no credentials",
            ask("/api/memory/stats", &[]).await?,
            NO_CREDENTIALS,
        ),
        (
            "guard, unknown token",
            ask(
                "/api/memory/stats",
                &[("authorization", "Bearer rhca_unknown")],
            )
            .await?,
            INVALID_TOKEN,
        ),
        (
            "guard, html page",
            ask("/", &[("accept", "text/html")]).await?,
            NO_CREDENTIALS,
        ),
        (
            "route, unknown token from a direct local caller",
            me(&node, &[("authorization", "Bearer rhca_unknown")]).await?,
            INVALID_TOKEN,
        ),
    ] {
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{label}");
        assert_eq!(
            answer.header("www-authenticate"),
            Some(challenge),
            "{label}"
        );
    }
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_loopback_is_not_the_owner() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    for header in [
        ("x-forwarded-for", "203.0.113.9"),
        ("forwarded", "for=203.0.113.9;proto=https"),
        ("x-real-ip", "203.0.113.9"),
        ("x-forwarded-host", "team.example.test"),
        ("via", "1.1 forwarder"),
    ] {
        let anonymous = me(&node, &[header]).await?;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{header:?}");
        let cleartext_token = me(&node, &[header, ("authorization", "Bearer alice-token")]).await?;
        assert_eq!(
            cleartext_token.status,
            StatusCode::UNAUTHORIZED,
            "{header:?} with a token over plain HTTP"
        );
    }
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trusted_proxy_passes_its_client_and_https() -> Outcome {
    let node = Node::start(Shape {
        trusted_proxies: &["127.0.0.1/32"],
        ..Shape::default()
    })
    .await?;
    let from = |client: &'static str| [("x-forwarded-for", client), ("x-forwarded-proto", "https")];
    let alice = me(
        &node,
        &[
            ("x-forwarded-for", "203.0.113.9"),
            ("x-forwarded-proto", "https"),
            ("authorization", "Bearer alice-token"),
        ],
    )
    .await?;
    assert_eq!(alice.status, StatusCode::OK);
    assert_eq!(user(&alice)?, "alice");
    let anonymous = me(&node, &from("203.0.113.9")).await?;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let cleartext = me(
        &node,
        &[
            ("x-forwarded-for", "203.0.113.9"),
            ("x-forwarded-proto", "http"),
            ("authorization", "Bearer alice-token"),
        ],
    )
    .await?;
    assert_eq!(cleartext.status, StatusCode::UNAUTHORIZED);
    let unforwarded = me(&node, &[]).await?;
    assert_eq!(
        unforwarded.status,
        StatusCode::UNAUTHORIZED,
        "a trusted proxy's own address is never the owner"
    );
    let client = node.client(LOOPBACK)?;
    let miss = |headers: [(&'static str, &'static str); 2]| {
        let client = client.clone();
        let url = format!("{}/api/drops/{}", node.base, fresh_id());
        async move { send(&client, Method::GET, &url, &headers, Payload::Empty).await }
    };
    for _ in 0..10 {
        assert_eq!(
            miss(from("203.0.113.9")).await?.status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        miss(from("203.0.113.9")).await?.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        miss(from("203.0.113.10")).await?.status,
        StatusCode::NOT_FOUND,
        "the limiter keys on the forwarded client, not the proxy"
    );
    node.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn me_is_the_token_check() -> Outcome {
    let node = Node::start(Shape::default()).await?;
    for (token, holder) in [("alice-token", "alice"), ("erin-token", "erin")] {
        let answer = me(&node, &[("authorization", &format!("Bearer {token}"))]).await?;
        assert_eq!(answer.status, StatusCode::OK, "{token}");
        assert_eq!(user(&answer)?, holder);
    }
    let unknown = me(&node, &[("authorization", "Bearer rhca_unknown")]).await?;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    node.stop().await
}

async fn stats(node: &Node, headers: &[(&str, &str)]) -> Result<Answer, Failure> {
    send(
        &node.client(LOOPBACK)?,
        Method::GET,
        &format!("{}/api/memory/stats", node.base),
        headers,
        Payload::Empty,
    )
    .await
}

fn assert_insufficient_scope(answer: &Answer, label: &str) -> Outcome {
    assert_eq!(answer.status, StatusCode::FORBIDDEN, "{label}");
    let body: GatewayError = serde_json::from_slice(&answer.body)?;
    assert_eq!(
        body.code.as_deref(),
        Some("insufficient_scope"),
        "{label}: {body:?}"
    );
    assert_eq!(
        answer.header("www-authenticate"),
        Some(r#"Bearer realm="rivethub", error="insufficient_scope", scope="full""#),
        "{label}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drops_only_token_shares_and_nothing_else() -> Outcome {
    let node = Node::start(Shape {
        trusted_proxies: &["127.0.0.1/32"],
        ..Shape::default()
    })
    .await?;
    let client = node.client(LOOPBACK)?;
    let proxied = [
        ("x-forwarded-for", "203.0.113.9"),
        ("x-forwarded-proto", "https"),
        ("authorization", "Bearer erin-token"),
    ];
    let created = send(
        &client,
        Method::POST,
        &format!("{}/api/drops", node.base),
        &proxied,
        Payload::Bytes(sealed(16)),
    )
    .await?;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "{:?}",
        created.refusal()
    );
    let id = serde_json::from_slice::<Uploaded>(&created.body)?.id;
    let revoked = send(
        &client,
        Method::DELETE,
        &format!("{}/api/drops/{id}", node.base),
        &proxied,
        Payload::Empty,
    )
    .await?;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert_insufficient_scope(&stats(&node, &proxied).await?, "through the proxy")?;
    let me = me(&node, &proxied).await?;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(user(&me)?, "erin");
    node.stop().await?;
    let local = Node::start(Shape::default()).await?;
    let direct = stats(&local, &[("authorization", "Bearer erin-token")]).await?;
    assert_insufficient_scope(&direct, "direct local")?;
    let full = stats(&local, &[("authorization", "Bearer alice-token")]).await?;
    assert_eq!(full.status, StatusCode::OK);
    local.stop().await
}

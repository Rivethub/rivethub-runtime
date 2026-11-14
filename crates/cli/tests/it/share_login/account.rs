use super::*;

#[tokio::test]
async fn node_account_and_cli_share_one_login_and_never_call_retired_routes() {
    use gateway::account::{AccountLogin as _, LoginProgress};
    let node = node();
    let served = serve().await;
    served.auth.queue("/api/auth/device/code", [device(1)]);
    served.auth.queue(
        "/api/auth/oauth2/token",
        [
            oauth_error("authorization_pending"),
            oauth_error("slow_down"),
            issued("account"),
        ],
    );
    served.auth.0.lock().me = Some(ok(r#"{"email":"you@example.com"}"#));
    let login = cli::account_login::NodeAccountLogin::new(
        node.env.clone(),
        &format!("http://{}/", served.host).parse().expect("url"),
    );
    assert!(login.identity().await.expect("signed out").is_none());
    let shown = login.start().await.expect("start");
    assert_eq!(shown.user_code, "ABCD-EFGH");
    loop {
        match login.poll().await.expect("poll") {
            LoginProgress::Pending => poll_interval().await,
            LoginProgress::SignedIn => break,
            other @ (LoginProgress::Expired | LoginProgress::Denied) => {
                panic!("unexpected {other:?}")
            }
        }
    }
    assert_eq!(
        login
            .identity()
            .await
            .expect("identity")
            .expect("signed in")
            .1,
        "you@example.com"
    );
    assert!(node.logins().contains("rhcr_account"), "same CLI store");
    served.auth.queue("/api/auth/oauth2/revoke", [ok("{}")]);
    desk::logout(&node.env, Some(&served.host))
        .await
        .expect("CLI logout");
    assert!(login.identity().await.expect("signed out").is_none());
    assert!(
        served
            .auth
            .paths()
            .iter()
            .all(|path| !path.starts_with("/v1/auth/"))
    );
    assert!(
        served
            .auth
            .seen()
            .iter()
            .filter(|seen| seen.path == "/v1/account")
            .all(|seen| seen.authorization == "Bearer rhca_account")
    );
}

#[tokio::test]
async fn node_account_reports_expiry_and_refusal_without_a_login() {
    use gateway::account::{AccountLogin as _, LoginProgress};
    for (code, denied) in [("expired_token", false), ("access_denied", true)] {
        let node = node();
        let served = serve().await;
        served.auth.queue("/api/auth/device/code", [device(1)]);
        served
            .auth
            .queue("/api/auth/oauth2/token", [oauth_error(code)]);
        let login = cli::account_login::NodeAccountLogin::new(
            node.env.clone(),
            &format!("http://{}/", served.host).parse().expect("url"),
        );
        login.start().await.expect("start");
        loop {
            match login.poll().await.expect("poll") {
                LoginProgress::Pending => poll_interval().await,
                LoginProgress::Denied if denied => break,
                LoginProgress::Expired if !denied => break,
                other @ (LoginProgress::SignedIn
                | LoginProgress::Expired
                | LoginProgress::Denied) => panic!("unexpected {other:?}"),
            }
        }
        assert!(login.identity().await.expect("identity").is_none());
    }
}

#[tokio::test]
async fn cli_login_refresh_and_cloud_failure_are_observed_by_the_node() {
    use gateway::account::AccountLogin as _;
    let node = node();
    let served = serve().await;
    served.auth.queue("/api/auth/device/code", [device(1)]);
    served
        .auth
        .queue("/api/auth/oauth2/token", [issued("original")]);
    assert!(matches!(
        signed_in(&node, &served.host).await.expect("CLI login"),
        Approval::SignedIn { .. }
    ));
    let login = cli::account_login::NodeAccountLogin::new(
        node.env.clone(),
        &format!("http://{}/", served.host).parse().expect("url"),
    );
    served.auth.0.lock().me = Some(ok(r#"{"email":"you@example.com"}"#));
    assert!(login.identity().await.expect("identity").is_some());
    let file = node.home.path().join("state/rivethub/share-logins.json");
    let text = std::fs::read_to_string(&file).expect("store");
    let mut records: crate::account_fixture::LoginFile =
        serde_json::from_str(&text).expect("records");
    records
        .logins
        .get_mut(&served.host)
        .expect("login")
        .expires_at = "2000-01-01T00:00:00Z".parse().expect("time");
    std::fs::write(&file, serde_json::to_vec(&records).expect("serialize")).expect("save");
    served
        .auth
        .queue("/api/auth/oauth2/token", [issued("refreshed")]);
    assert!(login.identity().await.expect("refresh").is_some());
    assert!(
        served
            .auth
            .seen()
            .iter()
            .any(|seen| seen.params.get("grant_type").map(String::as_str) == Some("refresh_token"))
    );
    served.auth.0.lock().me = Some(Reply {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: "private response must never escape".to_owned(),
    });
    let error = login.identity().await.expect_err("cloud failure");
    assert!(!error.to_string().contains("private response"));
    served.auth.0.lock().me = Some(Reply {
        status: StatusCode::UNAUTHORIZED,
        body: "{}".to_owned(),
    });
    assert!(login.identity().await.expect("revoked").is_none());
    assert!(login.identity().await.expect("forgotten").is_none());
}

async fn poll_interval() {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            sleep,
            reason = "CONTRACT note 155: this models the app polling the node's progress while the node owns OAuth timing"
        )
    )]
    tokio::time::sleep(Duration::from_millis(50)).await;
}

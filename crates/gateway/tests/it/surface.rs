use crate::common;

use common::gateway_with;
use gateway::Settings;

fn app(settings: Settings) -> axum::Router {
    gateway_with(settings).router(false)
}

fn call(router: axum::Router, target: &str) -> Result<testkit::Response, String> {
    let request = testkit::Request {
        method: "GET".to_owned(),
        target: target.to_owned(),
        headers: vec![("host".to_owned(), "127.0.0.1".to_owned())],
        body: Vec::new(),
        recipe: None,
        via: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("runtime: {error}"))?;
    runtime.block_on(common::answer(router, request))
}

#[test]
fn a_node_with_no_web_app_answers_the_root_with_a_page() {
    let router = app(common::settings());
    let page = call(router.clone(), "/").expect("root");
    assert_eq!(page.status, 200);
    let content_type = header(&page, "content-type");
    assert_eq!(content_type, "text/html; charset=utf-8");
    let text = String::from_utf8(page.body).expect("utf-8");
    assert!(text.contains("This RivetHub node is running"), "{text}");
    assert!(text.contains("The web app is not installed"), "{text}");
    assert!(!text.contains("{\"error\""));
    let head = call_method(router.clone(), "HEAD", "/").expect("head");
    assert_eq!(head.status, 200);
    assert_eq!(header(&head, "content-type"), "text/html; charset=utf-8");
    assert!(head.body.is_empty(), "{:?}", head.body);
    let missing = call(router.clone(), "/missing-page").expect("missing");
    assert_eq!(missing.status, 404);
    assert_eq!(missing.body, br#"{"error":"not found"}"#);
    let api = call(router, "/api/does-not-exist").expect("api");
    assert_eq!(api.status, 404);
    assert_eq!(api.body, br#"{"error":"not found"}"#);
}

fn header(response: &testkit::Response, name: &str) -> String {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn call_method(
    router: axum::Router,
    method: &str,
    target: &str,
) -> Result<testkit::Response, String> {
    let request = testkit::Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers: vec![("host".to_owned(), "127.0.0.1".to_owned())],
        body: Vec::new(),
        recipe: None,
        via: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("runtime: {error}"))?;
    runtime.block_on(common::answer(router, request))
}

#[test]
fn symlink_and_sibling_prefix_stay_inside_the_static_root() {
    let scratch = tempfile::tempdir().expect("temp");
    let root = scratch.path().join("static");
    let sibling = scratch.path().join("static-evil");
    std::fs::create_dir(&root).expect("root");
    std::fs::create_dir(&sibling).expect("sibling");
    std::fs::write(root.join("ok.txt"), b"yes").expect("ok");
    std::fs::write(sibling.join("secret.txt"), b"no").expect("secret");
    std::os::unix::fs::symlink(sibling.join("secret.txt"), root.join("escape")).expect("escape");
    std::os::unix::fs::symlink(root.join("ok.txt"), root.join("linked")).expect("linked");
    std::os::unix::fs::symlink(&sibling, root.join("out")).expect("out");
    let mut settings = common::settings();
    settings.static_dir = Some(root);
    let router = app(settings);
    let ok = call(router.clone(), "/ok.txt").expect("ok");
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, b"yes");
    let linked = call(router.clone(), "/linked").expect("linked");
    assert_eq!(linked.status, 200);
    assert_eq!(linked.body, b"yes");
    let escape = call(router.clone(), "/escape").expect("escape");
    assert_eq!(escape.status, 404);
    assert_eq!(escape.body, br#"{"error":"not found"}"#);
    let dotdot = call(router.clone(), "/foo/../ok.txt").expect("dotdot");
    assert_eq!(dotdot.status, 404);
    assert_eq!(dotdot.body, br#"{"error":"not found"}"#);
    let nested = call(router, "/out/secret.txt").expect("nested");
    assert_eq!(nested.status, 404);
    assert_eq!(nested.body, br#"{"error":"not found"}"#);
}

#[test]
fn app_deep_links_get_the_shell_and_removed_live_routes_answer_404() {
    let scratch = tempfile::tempdir().expect("temp");
    std::fs::write(scratch.path().join("index.html"), b"<!doctype html>shell").expect("index");
    let mut settings = common::settings();
    settings.static_dir = Some(scratch.path().to_owned());
    let router = app(settings);
    for path in ["/sessions", "/sessions/abc", "/review", "/settings/account"] {
        let page = call(router.clone(), path).expect(path);
        assert_eq!((path, page.status), (path, 200));
        assert_eq!(page.body, b"<!doctype html>shell");
    }
    for path in [
        "/ws",
        "/session",
        "/state",
        "/event",
        "/events",
        "/mesh.json",
        "/term/main",
        "/files",
        "/files/a.txt",
        "/ca/v1/chain",
        "/api/tasks",
    ] {
        let gone = call(router.clone(), path).expect(path);
        assert_eq!((path, gone.status), (path, 404));
    }
}

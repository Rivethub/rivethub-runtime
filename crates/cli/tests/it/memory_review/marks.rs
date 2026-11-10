use cli::Exit;

use super::Model;
use super::Node;
use super::serve;

#[tokio::test]
async fn a_mark_keeps_underscores_and_stars_inside_its_terms() {
    let model = Model::scoring(&[("halyard", 0.9), ("Acme Corp", 0.8)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw(
        "s.json",
        &[&["use halyard_app for the *prod* checks", "Acme Corp churns"]],
    );
    let sanitized = node.sanitize(&raw, &["--decide"]).await;
    sanitized.mark(1, "**Mark:** map `halyard_app` as DB_USER");
    sanitized.mark(2, "Mark: *redact*");

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Ok, "{}", resolve.stderr);
    assert_eq!(
        sanitized.contents(),
        [vec![
            "use DB_USER for the *prod* checks",
            "[REDACTED:flagged]"
        ]]
    );
    stub.close().await.expect("stub shuts down");
}

async fn refused_mark(mark: &str, learn: bool) -> String {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw("s.json", &[&["Acme Corp is churning next quarter."]]);
    let sanitized = node.sanitize(&raw, &["--decide"]).await;
    let before = sanitized.bundle_text();
    sanitized.mark(1, mark);
    let extra: &[&str] = if learn { &["--learn"] } else { &[] };

    let resolve = node.resolve(&sanitized, extra).await;

    assert_eq!(resolve.exit, Exit::Failure, "{mark}: {}", resolve.stdout);
    assert_eq!(sanitized.bundle_text(), before, "{mark}");
    assert_eq!(
        std::fs::read_to_string(node.path("rules.toml")).expect("mapping"),
        "",
        "{mark}"
    );
    stub.close().await.expect("stub shuts down");
    resolve.stderr
}

#[tokio::test]
async fn a_map_target_may_not_be_a_placeholder_or_a_secret() {
    for target in [
        "[FLAGGED:3]",
        "x [REDACTED:secret_key]",
        concat!("ghp_", "PLANTEDmapZq8RmT2vLx9WabcdefghijklmnoQ"),
    ] {
        let stderr = refused_mark(&format!("Mark: map Acme as {target}"), false).await;
        assert!(stderr.contains("flag 1"), "{target}: {stderr}");
    }
}

#[tokio::test]
async fn a_map_term_may_not_be_blank_or_cut_into_a_placeholder() {
    refused_mark("Mark: map \" \" as -", false).await;
    let model = Model::scoring(&[("api_key", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw("s.json", &[&["api_key: PLANTEDassignCO07ab"]]);
    let sanitized = node.sanitize(&raw, &["--decide"]).await;
    assert!(
        sanitized
            .report()
            .contains("api_key: [REDACTED:secret_assignment]")
    );
    sanitized.mark(1, "Mark: map DACTED:secre as [X]");

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn learn_refuses_a_term_too_short_to_be_a_rule() {
    let stderr = refused_mark("Mark: map Ac as X", true).await;
    assert!(stderr.contains("flag 1"), "{stderr}");
}

#[tokio::test]
async fn a_learned_word_spares_longer_words_that_start_with_it() {
    let model = Model::scoring(&[("Tom approved", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw("s.json", &[&["Tom approved the Tomcat upgrade"]]);
    let sanitized = node.sanitize(&raw, &["--decide"]).await;
    sanitized.mark(1, "Mark: map Tom as PERSON");

    let resolve = node.resolve(&sanitized, &["--learn"]).await;

    assert_eq!(resolve.exit, Exit::Ok, "{}", resolve.stderr);
    assert_eq!(
        sanitized.contents(),
        [vec!["PERSON approved the Tomcat upgrade"]]
    );
    let later = node.raw("later.json", &[&["Tomcat and Tomas met Tom"]]);
    let plain = node.sanitize(&later, &[]).await;
    assert_eq!(plain.contents(), [vec!["Tomcat and Tomas met PERSON"]]);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_mapping_rule_may_not_write_a_flag_placeholder() {
    let node = Node::new("http://127.0.0.1:9", 5);
    std::fs::write(
        node.path("rules.toml"),
        "[[rule]]\nliteral = \"Acme\"\naction = \"map\"\nas = \"[FLAGGED:3]\"\n",
    )
    .expect("mapping");
    let raw = node.raw("s.json", &[&["Acme Corp is churning."]]);
    let out = node.path("s.clean.json");
    let rules = node.path("rules.toml");

    let sanitize = node
        .run(&[
            "memory",
            "sanitize",
            &raw,
            "--out",
            &out,
            "--mapping",
            &rules,
        ])
        .await;

    assert_eq!(sanitize.exit, Exit::Failure, "{}", sanitize.stdout);
    assert!(sanitize.stderr.contains("line 1"), "{}", sanitize.stderr);
}

#[tokio::test]
async fn a_flag_placeholder_written_in_a_session_is_escaped_reversibly() {
    let node = Node::new("http://127.0.0.1:9", 5);
    let raw = node.raw("s.json", &[&["quote [FLAGGED:012] and [FLAGGED::7] here"]]);

    let sanitized = node.sanitize(&raw, &[]).await;

    assert_eq!(
        sanitized.contents(),
        [vec!["quote [FLAGGED::012] and [FLAGGED:::7] here"]]
    );
    assert!(
        sanitized
            .stdout
            .starts_with("sanitized 1 session: 0 secrets, 0 mapped, 0 redacted, 2 escaped;"),
        "{}",
        sanitized.stdout
    );
    assert!(
        node.ingest_line(&sanitized)
            .await
            .ends_with("would ingest 1 session from phil; nothing written")
    );
}

#[tokio::test]
async fn the_report_warns_that_it_holds_flagged_text() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let raw = node.raw("s.json", &[&["Acme Corp is churning next quarter."]]);

    let sanitized = node.sanitize(&raw, &["--decide", "--block"]).await;

    let report = sanitized.report();
    let paragraphs: Vec<&str> = report.split("\n\n").collect();
    assert!(
        paragraphs
            .get(1)
            .is_some_and(|first| first.starts_with("The sanitizer is best-effort.")),
        "{report}"
    );
    assert!(report.contains("Never share this report"), "{report}");
    stub.close().await.expect("stub shuts down");
}

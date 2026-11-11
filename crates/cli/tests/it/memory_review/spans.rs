use cli::Exit;

use super::Model;
use super::Node;
use super::Sanitized;
use super::serve;

const PLAN: &str = "Globex plan";
const DUE: &str = "Globex plan is due";

fn filler(length: usize) -> String {
    "status ok ".repeat(length / 10 + 1)[..length].to_owned()
}

fn section(report: &str, number: usize) -> (usize, usize) {
    let heading = format!("### Flag {number} ");
    let start = report.find(&heading).expect("flag section");
    let end = report[start + heading.len()..]
        .find("\n### Flag ")
        .or_else(|| report[start + heading.len()..].find("\n## "))
        .map_or(report.len(), |offset| start + heading.len() + offset + 1);
    (start, end)
}

fn without_section(report: &str, number: usize) -> String {
    let (start, end) = section(report, number);
    format!("{}{}", &report[..start], &report[end..])
}

fn duplicated(report: &str, number: usize, copy: usize) -> String {
    let (start, end) = section(report, number);
    let renamed = report[start..end]
        .replace(&format!("### Flag {number} "), &format!("### Flag {copy} "))
        .replace(
            &format!("<!-- flag {number}: "),
            &format!("<!-- flag {copy}: "),
        );
    format!("{}{renamed}{}", &report[..end], &report[end..])
}

async fn resolved(node: &Node, sanitized: &Sanitized) {
    let resolve = node.resolve(sanitized, &[]).await;
    assert_eq!(resolve.exit, Exit::Ok, "{}", resolve.stderr);
}

#[tokio::test]
async fn a_text_flagged_in_one_session_is_withheld_in_every_session() {
    let model = Model::scoring(&[("Initech layoffs", 0.8), ("Globex merger", 0.95)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 1);
    let layoffs = "Initech layoffs are planned for March.";
    let raw = node.raw(
        "dup.json",
        &[&[layoffs], &["Globex merger closes Friday.", layoffs]],
    );

    let sanitized = node.sanitize(&raw, &["--decide", "--block"]).await;

    assert!(
        !sanitized.bundle_text().contains("Initech layoffs"),
        "{}",
        sanitized.bundle_text()
    );
    let report = sanitized.report();
    let mut all = report.clone();
    for number in 1..=report.matches("\nMark: open").count() {
        all = super::marked(&all, number, "Mark: keep");
    }
    sanitized.write_report(&all);
    resolved(&node, &sanitized).await;
    assert_eq!(
        sanitized.contents(),
        [vec![layoffs], vec!["Globex merger closes Friday.", layoffs]]
    );
    assert_eq!(sanitized.unresolved(), Some(0));
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn marks_on_flags_whose_texts_overlap_each_apply_and_a_rerun_changes_nothing() {
    let model = Model::scoring(&[("Globex", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let line = filler(1195);
    let content = format!("{PLAN}\n{line}\n{DUE}");
    let raw = node.raw("edge.json", &[&[&content]]);
    let sanitized = node.sanitize(&raw, &["--decide"]).await;
    assert!(sanitized.report().contains(&format!("~~~text\n{DUE}\n~~~")));

    sanitized.mark(1, "Mark: map plan as <p>");
    resolved(&node, &sanitized).await;
    sanitized.mark(2, "Mark: redact");
    resolved(&node, &sanitized).await;

    assert_eq!(
        sanitized.contents(),
        [vec![format!("Globex <p>\n{line}\n[REDACTED:flagged]")]]
    );
    assert_eq!(sanitized.unresolved(), Some(0));
    let (bundle, report) = (sanitized.bundle_text(), sanitized.report());
    resolved(&node, &sanitized).await;
    assert_eq!(sanitized.bundle_text(), bundle);
    assert_eq!(sanitized.report(), report);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_redact_after_a_map_in_the_same_field_still_applies() {
    let model = Model::scoring(&[("Initech", 0.9), ("lorem", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let lorem = "lorem ipsum dolor sit amet ".repeat(50)[..1185].to_owned();
    let raw = node.raw(
        "overlap.json",
        &[&[&format!("Initech ledger\n{lorem}\nInitech ledger")]],
    );
    let sanitized = node.sanitize(&raw, &["--decide"]).await;

    sanitized.mark(1, "Mark: map Initech as [ORG]");
    sanitized.mark(2, "Mark: redact");
    resolved(&node, &sanitized).await;

    assert_eq!(
        sanitized.contents(),
        [vec!["[ORG] ledger\n[REDACTED:flagged]".to_owned()]]
    );
    stub.close().await.expect("stub shuts down");
}

async fn churn(node: &Node, mode: &[&str]) -> Sanitized {
    let raw = node.raw(
        "s.json",
        &[&[
            "Added a parser test; all 42 passed.",
            "Acme Corp is churning next quarter.",
        ]],
    );
    node.sanitize(&raw, mode).await
}

#[tokio::test]
async fn an_edited_flag_text_is_refused_and_points_to_map() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = churn(&node, &["--decide", "--block"]).await;
    let before = sanitized.bundle_text();
    sanitized.write_report(&sanitized.report().replace(
        "Acme Corp is churning next quarter.\n~~~",
        "Acme Corp is fine.\n~~~",
    ));
    sanitized.mark(1, "Mark: keep");

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    assert!(resolve.stderr.contains("flag 1"), "{}", resolve.stderr);
    assert!(resolve.stderr.contains("map"), "{}", resolve.stderr);
    assert_eq!(sanitized.bundle_text(), before);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_forged_hidden_copy_is_refused() {
    use base64::Engine as _;
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = churn(&node, &["--decide", "--block"]).await;
    let before = sanitized.bundle_text();
    let report = sanitized.report();
    let hidden = report
        .split('\n')
        .find_map(|line| line.strip_prefix("<!-- flag 1: "))
        .and_then(|rest| rest.split(' ').next())
        .expect("hidden copy")
        .to_owned();
    let forged = base64::engine::general_purpose::STANDARD.encode("injected text");
    sanitized.write_report(&report.replace(&hidden, &forged));
    sanitized.mark(1, "Mark: keep");

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    assert!(resolve.stderr.contains("flag 1"), "{}", resolve.stderr);
    assert_eq!(sanitized.bundle_text(), before);
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn deleting_a_flag_section_leaves_the_flag_unreviewed() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = churn(&node, &["--decide"]).await;
    sanitized.write_report(&without_section(&sanitized.report(), 1));

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Ok, "{}", resolve.stderr);
    assert!(
        resolve.stdout.contains("1 flag still unreviewed"),
        "{}",
        resolve.stdout
    );
    assert_eq!(sanitized.unresolved(), Some(1));
    assert!(
        node.ingest_line(&sanitized)
            .await
            .ends_with("(1 advisory flags unreviewed); nothing written"),
    );
    stub.close().await.expect("stub shuts down");
}

#[tokio::test]
async fn a_mark_for_a_flag_the_bundle_does_not_have_is_refused() {
    let model = Model::scoring(&[("Acme Corp", 0.9)]);
    let stub = serve(&model).await;
    let node = Node::new(&format!("http://{}", stub.addr), 5);
    let sanitized = churn(&node, &["--decide"]).await;
    let before = sanitized.bundle_text();
    sanitized.write_report(&super::marked(
        &duplicated(&sanitized.report(), 1, 9),
        9,
        "Mark: redact",
    ));

    let resolve = node.resolve(&sanitized, &[]).await;

    assert_eq!(resolve.exit, Exit::Failure, "{}", resolve.stdout);
    assert!(resolve.stderr.contains("flag 9"), "{}", resolve.stderr);
    assert_eq!(sanitized.bundle_text(), before);
    stub.close().await.expect("stub shuts down");
}

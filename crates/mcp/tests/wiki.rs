#![cfg(test)]

use kernel::memory::Provenance;
use kernel::wiki::WikiFilter;
use kernel::wiki::page::WikiHistoryEntry;
use kernel::wiki::{WikiCitation, WikiPageResponse};
use mcp::Server;
use rmcp::model::CallToolResult;
use std::sync::Arc;
use testkit::{FakeMemoryStore, MemoryRequest};
use tokio_util::sync::CancellationToken;

pub mod support;

fn setup() -> (Server, Arc<FakeMemoryStore>) {
    let store = Arc::new(FakeMemoryStore::default());
    let server = Server::new(store.clone(), false, CancellationToken::new()).expect("server");
    (server, store)
}

const MARKDOWN: &str = "---\ntitle: Acme 日本語\nslug: acme\naliases:\n  - AcmeKit\n  - Acme's $(tool)\ntags:\n  - project\nlast_verified: 2026-10-14T00:00:00Z\n---\n\n## Summary\n\nReady 🦀\n\n## Article\n\nArticle 日本語\n\n## See also\n\n- [[related-topic]]\n\n## History\n\n### 2026-10-14 — Ready\n\nRecent history\n\n### 2026-10-01 — Started\n\nOlder history\n\n## Citations\n\n| Date | Kind | Summary | Note |\n|------|------|---------|------|\n| 2026-10-14 | leaf | `00000000-0000-4000-8000-000000000001` | Decision 🦀 |\n";

const HEADER: &str =
    "---\ntitle: Acme 日本語\nslug: acme\nlast_verified: 2026-10-14T00:00:00Z\ntags: project";

fn page() -> WikiPageResponse {
    WikiPageResponse {
        slug: "acme".into(),
        title: "Acme 日本語".into(),
        aliases: vec!["AcmeKit".into(), "Acme's $(tool)".into()],
        tags: vec!["project".into()],
        entities: vec!["example.com".into()],
        current_state: "Ready 🦀".into(),
        article: Some("Article 日本語".into()),
        see_also: Some(vec!["related-topic".into()]),
        history: vec![
            WikiHistoryEntry {
                date: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).expect("date"),
                title: "Started".into(),
                body: "Older history".into(),
            },
            WikiHistoryEntry {
                date: chrono::NaiveDate::from_ymd_opt(2026, 10, 14).expect("date"),
                title: "Ready".into(),
                body: "Recent history".into(),
            },
        ],
        citations: vec![WikiCitation {
            summary_id: "00000000-0000-4000-8000-000000000001".into(),
            date: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-14T00:00:00Z")
                    .expect("instant")
                    .with_timezone(&chrono::Utc),
            ),
            kind: Some("summary".into()),
            note: Some("Decision 🦀".into()),
            conversation_id: None,
            session_id: None,
            title: None,
            channel: None,
            topic_id: None,
            imported: None,
        }],
        markdown: MARKDOWN.into(),
        sources: vec![],
        git_sha: None,
        last_verified: None,
        updated_at: chrono::DateTime::parse_from_rfc3339("2026-10-14T00:00:00Z")
            .expect("instant")
            .with_timezone(&chrono::Utc),
        related: None,
        topic_id: None,
    }
}

fn text(result: &CallToolResult) -> &str {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert_eq!(result.content.len(), 1);
    &result
        .content
        .first()
        .expect("content")
        .as_text()
        .expect("text result")
        .text
}

#[tokio::test]
async fn search_preserves_typed_results_and_validates_limits() {
    let (server, store) = setup();
    let page = page();
    store.insert_wiki_page(page).await;
    for (arguments, limit) in [
        (r#"{"query":"日本語"}"#, 5),
        (r#"{"query":"日本語","limit":1}"#, 1),
        (r#"{"query":"日本語","limit":20}"#, 20),
    ] {
        let result = support::call(&server, "wiki_search", arguments).await;
        assert_eq!(
            text(&result),
            "## Acme 日本語 (acme) — verified 2026-10-14\nReady 🦀"
        );
        assert_eq!(
            store.requests().await.last_chunk::<2>(),
            Some(&[
                MemoryRequest::WikiTopics(WikiFilter {
                    query: Some("日本語".into()),
                    tag: None,
                    limit,
                    offset: 0,
                }),
                MemoryRequest::WikiImportedAuthors(vec!["acme".into()]),
            ])
        );
    }
    let result = support::call(&server, "wiki_search", r#"{"query":"missing"}"#).await;
    assert_eq!(text(&result), "No wiki topics match \"missing\".");
}

#[tokio::test]
async fn read_uses_typed_sections_and_preserves_small_markdown() {
    let (server, store) = setup();
    let page = page();
    store.insert_wiki_page(page.clone()).await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"acme"}"#).await;
    assert_eq!(text(&result), page.markdown);
    let aliases = "aliases: 2 (AcmeKit, Acme's $(tool))";
    for (section, alias_line, expected) in [
        ("summary", aliases, "## Summary\n\nReady 🦀"),
        ("article", aliases, "## Article\n\nArticle 日本語"),
        (
            "aliases",
            "aliases_count: 2",
            "## Aliases (2 of 2)\n\n- AcmeKit\n- Acme's $(tool)",
        ),
        (
            "history",
            aliases,
            "## History (2 of 2, newest first)\n\n### 2026-10-14 — Ready\n\nRecent history\n\n### 2026-10-01 — Started\n\nOlder history",
        ),
        (
            "citations",
            aliases,
            "## Citations (1 of 1)\n\n| Date | Kind | Summary | Note |\n|------|------|---------|------|\n| 2026-10-14 | leaf | `00000000-0000-4000-8000-000000000001` | Decision 🦀 |",
        ),
    ] {
        let result = support::call(
            &server,
            "wiki_read",
            &format!(r#"{{"slug":"acme","section":"{section}"}}"#),
        )
        .await;
        assert_eq!(
            text(&result),
            format!("section={section}\n\n{HEADER}\n{alias_line}\n---\n\n{expected}")
        );
    }
    let result = support::call(&server, "wiki_read", r#"{"slug":"acme","section":"full"}"#).await;
    assert_eq!(text(&result), page.markdown);
    assert_eq!(
        store.requests().await,
        vec![MemoryRequest::WikiPage("acme".into()); 7]
    );
}

fn imported(author: &str, verification: &str) -> Provenance {
    Provenance {
        team_owner: None,
        author: Some(author.to_owned()),
        origin_date: None,
        verification: match verification {
            "verified" => kernel::memory::Verification::Verified,
            "unsigned" => kernel::memory::Verification::Unsigned,
            other => panic!("unexpected verification {other}"),
        },
        checks: None,
    }
}

fn citing(summary: &str, imported: Option<Provenance>) -> WikiCitation {
    WikiCitation {
        summary_id: summary.into(),
        imported,
        ..WikiCitation {
            summary_id: String::new(),
            date: None,
            kind: Some("leaf".into()),
            note: None,
            conversation_id: None,
            session_id: None,
            title: None,
            channel: None,
            topic_id: None,
            imported: None,
        }
    }
}

#[tokio::test]
async fn citations_name_imported_authors() {
    let (server, store) = setup();
    let native = page();
    let mut mixed = page();
    mixed.slug = "mixed".into();
    mixed.markdown = MARKDOWN.replace("slug: acme", "slug: mixed").replace(
        "| Decision 🦀 |\n",
        "| Decision 🦀 |\n| 2026-10-15 | leaf | `00000000-0000-4000-8000-000000000002` | Second |\n| 2026-10-16 | leaf | `00000000-0000-4000-8000-000000000003` | Third |\n",
    );
    mixed.citations = vec![
        citing(
            "00000000-0000-4000-8000-000000000001",
            Some(imported("phil", "unsigned")),
        ),
        citing(
            "00000000-0000-4000-8000-000000000002",
            Some(imported("alex", "verified")),
        ),
        citing("00000000-0000-4000-8000-000000000003", None),
    ];
    store.insert_wiki_page(native.clone()).await;
    store.insert_wiki_page(mixed.clone()).await;

    let read = support::call(&server, "wiki_read", r#"{"slug":"mixed"}"#).await;
    let view = text(&read);
    assert!(view.starts_with(
        "Built partly from imported sessions: phil (unverified), alex (verified).\n\n---\n"
    ));
    assert!(view.contains("| Decision 🦀 (imported from phil, unverified) |"));
    assert!(view.contains("| Second (imported from alex, verified) |"));
    assert!(view.contains("| Third |"));
    assert_eq!(view.matches("imported from").count(), 2);

    let read = support::call(&server, "wiki_read", r#"{"slug":"acme"}"#).await;
    assert_eq!(text(&read), native.markdown);

    let search = support::call(&server, "wiki_search", r#"{"query":"日本語"}"#).await;
    assert_eq!(
        text(&search),
        "## Acme 日本語 (acme) — verified 2026-10-14\nReady 🦀\n\n## Acme 日本語 (mixed) — verified 2026-10-14\nBuilt partly from imported sessions: phil (unverified), alex (verified).\nReady 🦀"
    );
    let requests = store.requests().await;
    let lookups = requests
        .iter()
        .filter(|request| matches!(request, MemoryRequest::WikiImportedAuthors(_)))
        .count();
    assert_eq!(lookups, 1);
}

#[tokio::test]
async fn oversized_views_and_sections_count_unicode_characters() {
    let (server, store) = setup();
    let mut page = page();
    let padding = 24_000 - page.markdown.chars().count() - "  - \n".len();
    page.markdown =
        page.markdown
            .replacen("tags:", &format!("  - {}\ntags:", "🦀".repeat(padding)), 1);
    assert_eq!(page.markdown.chars().count(), 24_000);
    store.insert_wiki_page(page.clone()).await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"acme","section":"full"}"#).await;
    assert_eq!(text(&result), page.markdown);
    let aliases = (0..400)
        .map(|index| format!("  - alias {index}\n"))
        .collect::<Vec<_>>()
        .concat();
    page.markdown = page
        .markdown
        .replacen("tags:", &format!("{aliases}tags:"), 1);
    store.insert_wiki_page(page.clone()).await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"acme"}"#).await;
    let view = text(&result);
    assert!(view.starts_with("⚠ Oversized wiki page \"acme\" ("));
    assert!(view.contains("aliases: 403 (showing 8: AcmeKit, Acme's $(tool), "));
    assert!(view.contains("## Summary\n\nReady 🦀"));
    assert!(view.contains("## Article\n\nArticle 日本語"));
    assert!(view.contains("- [[related-topic]]"));
    assert!(view.contains("## Recent history (2 of 2)"));
    assert!(!view.contains("alias 399"));
    let result = support::call(&server, "wiki_read", r#"{"slug":"acme","section":"full"}"#).await;
    assert_eq!(result.is_error, Some(true));
    let result = support::call(
        &server,
        "wiki_read",
        r#"{"slug":"acme","section":"aliases"}"#,
    )
    .await;
    assert!(text(&result).contains("## Aliases (200 of 403)\n\n- AcmeKit\n- Acme's $(tool)\n"));
    assert!(
        text(&result)
            .ends_with("\n…[203 more — wiki_read slug=acme section=aliases (first 200 only)]")
    );
}

#[tokio::test]
async fn redirects_resolve_and_cycles_invalid_targets_and_missing_pages_fail() {
    let (server, store) = setup();
    store.insert_wiki_page(page()).await;
    store
        .insert_wiki_redirect("old".into(), "alias".into())
        .await;
    store
        .insert_wiki_redirect("alias".into(), "acme".into())
        .await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"old"}"#).await;
    assert_eq!(text(&result), page().markdown);
    assert_eq!(
        store.requests().await,
        ["old", "alias", "acme"].map(|slug| MemoryRequest::WikiPage(slug.into()))
    );
    for target in [
        "../outside",
        "/absolute",
        "..",
        "bad\\path",
        "old",
        "missing",
    ] {
        store
            .insert_wiki_redirect("alias".into(), target.into())
            .await;
        assert_eq!(
            support::call(&server, "wiki_read", r#"{"slug":"old"}"#)
                .await
                .is_error,
            Some(true),
            "{target}"
        );
    }
    for index in 0..17 {
        store
            .insert_wiki_redirect(format!("hop-{index}"), format!("hop-{}", index + 1))
            .await;
    }
    assert_eq!(
        support::call(&server, "wiki_read", r#"{"slug":"hop-0"}"#)
            .await
            .is_error,
        Some(true)
    );
}

#[tokio::test]
async fn invalid_arguments_never_reach_the_port() {
    let (server, store) = setup();
    for args in [
        "{}",
        r#"{"query":7}"#,
        r#"{"query":false}"#,
        r#"{"query":[]}"#,
        r#"{"query":null}"#,
        r#"{"query":"x","limit":0}"#,
        r#"{"query":"x","limit":21}"#,
        r#"{"query":"x","limit":1.5}"#,
        r#"{"query":"x","limit":"5"}"#,
        r#"{"query":"x","unknown":true}"#,
    ] {
        assert_eq!(
            support::call(&server, "wiki_search", args).await.is_error,
            Some(true),
            "{args}"
        );
    }
    for args in [
        "{}",
        r#"{"slug":7}"#,
        r#"{"slug":null}"#,
        r#"{"slug":""}"#,
        r#"{"slug":"../sibling"}"#,
        r#"{"slug":".."}"#,
        r#"{"slug":"/absolute"}"#,
        r#"{"slug":"a\\b"}"#,
        r#"{"slug":"a\u0000b"}"#,
        r#"{"slug":"acme","section":"invalid"}"#,
        r#"{"slug":"acme","section":true}"#,
        r#"{"slug":"acme","unknown":true}"#,
    ] {
        assert_eq!(
            support::call(&server, "wiki_read", args).await.is_error,
            Some(true),
            "{args}"
        );
    }
    assert_eq!(store.requests().await, Vec::<MemoryRequest>::new());
}

#[tokio::test]
async fn valid_literal_slugs_and_absent_sections_are_handled_explicitly() {
    let (server, store) = setup();
    let mut page = page();
    page.slug = "Acme's $(topic) 日本語".into();
    page.markdown = "---\ntitle: Acme 日本語\nslug: acme\n---\n\n## Summary\n\nReady 🦀\n".into();
    store.insert_wiki_page(page.clone()).await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"Acme's $(topic) 日本語"}"#).await;
    assert_eq!(text(&result), page.markdown);
    for (section, expected) in [
        ("article", "## Article\n\n(empty)"),
        ("history", "## History (0 of 0, newest first)\n\n(none yet)"),
        ("aliases", "## Aliases (0 of 0)\n\n(none)"),
        ("citations", "## Citations (0 of 0)\n\n(none)"),
    ] {
        let result = support::call(
            &server,
            "wiki_read",
            &format!(r#"{{"slug":"Acme's $(topic) 日本語","section":"{section}"}}"#),
        )
        .await;
        assert!(
            text(&result).starts_with(&format!("section={section}\n\n---\ntitle: Acme 日本語")),
            "{section}"
        );
        assert!(text(&result).ends_with(expected), "{section}");
    }
    page.markdown = "---\ntitle: Acme 日本語\n---\n".into();
    store.insert_wiki_page(page).await;
    let result = support::call(&server, "wiki_read", r#"{"slug":"Acme's $(topic) 日本語"}"#).await;
    assert_eq!(
        text(&result),
        "⚠ Page \"Acme's $(topic) 日本語\" is malformed (invalid wiki page: slug required) — raw content follows; the next extractor pass will re-structure it.\n\n---\ntitle: Acme 日本語\n---\n"
    );
}

#[tokio::test]
async fn cancellation_prevents_wiki_port_calls() {
    let (server, store) = setup();
    server.shutdown();
    for (name, args) in [
        ("wiki_search", r#"{"query":"acme"}"#),
        ("wiki_read", r#"{"slug":"acme"}"#),
    ] {
        let result = support::call(&server, name, args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .first()
                .expect("content")
                .as_text()
                .expect("error text")
                .text,
            "operation cancelled"
        );
    }
    assert_eq!(store.requests().await, Vec::<MemoryRequest>::new());
}

#[tokio::test]
async fn store_failures_are_tool_errors_and_search_totals_are_not_page_lengths() {
    let (server, store) = setup();
    for (name, args) in [
        ("wiki_search", r#"{"query":"acme"}"#),
        ("wiki_read", r#"{"slug":"acme"}"#),
    ] {
        store
            .fail_next_wiki_request(kernel::memory::MemoryStoreError::InvalidRequest {
                message: "wiki store rejected the request".into(),
            })
            .await;
        let result = support::call(&server, name, args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .first()
                .expect("content")
                .as_text()
                .expect("error text")
                .text,
            "wiki store rejected the request"
        );
    }
    store.insert_wiki_page(page()).await;
    let mut second = page();
    second.slug = "acme-next".into();
    store.insert_wiki_page(second).await;
    let result = support::call(&server, "wiki_search", r#"{"query":"acme","limit":1}"#).await;
    assert_eq!(
        text(&result),
        "## Acme 日本語 (acme) — verified 2026-10-14\nReady 🦀"
    );
}

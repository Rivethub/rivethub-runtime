#![cfg(test)]

use kernel::wiki::page::{
    CitationKind, WikiPageError, build_wiki_search_text, parse_wiki_page, serialize_wiki_page,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn vendored_pages_round_trip() -> TestResult {
    for name in ["kiwi-build-cache.txt", "lighthouse-dashboard.txt"] {
        let page = parse_wiki_page(&std::fs::read_to_string(testkit::recording(&format!(
            "mcp-memory/shared/wiki/topics/{name}"
        )))?)?;
        let encoded = serialize_wiki_page(&page)?;
        assert_eq!(parse_wiki_page(&encoded)?, page, "{name}");
        assert_eq!(serialize_wiki_page(&parse_wiki_page(&encoded)?)?, encoded);
        assert_eq!(page.history.len(), 1);
        assert_ne!(page.citations, Vec::new());
        assert!(build_wiki_search_text(&page).contains(&page.current_state));
    }
    Ok(())
}

#[test]
fn legacy_condensed_citations_keep_their_kind() -> TestResult {
    let page = parse_wiki_page(&std::fs::read_to_string(testkit::recording(
        "mcp-memory/shared/wiki/topics/lighthouse-dashboard.txt",
    ))?)?;
    assert_eq!(
        page.citations
            .iter()
            .map(|citation| citation.kind)
            .collect::<Vec<_>>(),
        [Some(CitationKind::Condensed)]
    );
    Ok(())
}

#[test]
fn frontmatter_extras_preamble_sections_and_markdown_fences_survive() -> TestResult {
    let markdown = "---\ntitle: Notes\nslug: notes\ncustom:\n  owner: team\n  count: 3\n---\n\nA human preamble.\n\n## Summary\n\nUseful summary.\n\n## Article\n\n```markdown\n## History\n```\n\n## Troubleshooting\n\nKeep this section too.\n";
    let page = parse_wiki_page(markdown)?;
    assert_eq!(page.preamble.as_deref(), Some("A human preamble."));
    assert!(page.article.contains("## History"));
    assert_eq!(
        page.extra_sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect::<Vec<_>>(),
        ["Troubleshooting"]
    );
    let expected: values::Extra = serde_json::from_str(r#"{"custom":{"owner":"team","count":3}}"#)?;
    assert_eq!(page.meta.extra, expected);
    assert_eq!(parse_wiki_page(&serialize_wiki_page(&page)?)?, page);
    Ok(())
}

#[test]
fn topic_sections_are_typed_frontmatter_and_round_trip() -> TestResult {
    let markdown = "---\ntitle: Mesh\nslug: mesh\naliases: []\ntags: []\nentities: []\nrelated: []\nsources: []\nsections:\n  - topic: 00000000-0000-4000-8000-000000000001\n    heading: Certificates\n---\n\n## Summary\n\n\n\n\n\n## History\n\n\n\n\n## Certificates\n\nRenewal notes.\n";
    let page = parse_wiki_page(markdown)?;
    assert_eq!(
        page.meta.sections,
        [kernel::wiki::page::WikiSectionRef {
            topic: "00000000-0000-4000-8000-000000000001".to_owned(),
            heading: "Certificates".to_owned(),
        }]
    );
    assert_eq!(page.meta.extra, values::Extra::default());
    assert_eq!(serialize_wiki_page(&page)?, markdown);
    let plain = markdown.replace(
        "sections:\n  - topic: 00000000-0000-4000-8000-000000000001\n    heading: Certificates\n",
        "",
    );
    let page = parse_wiki_page(&plain)?;
    assert_eq!(page.meta.sections, Vec::new());
    assert_eq!(serialize_wiki_page(&page)?, plain);
    Ok(())
}

#[test]
fn citation_notes_with_pipes_survive_a_write_and_read() -> TestResult {
    let markdown = "---\ntitle: Notes\nslug: notes\n---\n\n## Summary\n\nText.\n\n## Citations\n\n| Date | Kind | Summary | Note |\n|------|------|---------|------|\n| 2026-10-10 | leaf | `00000000-0000-4000-8000-000000000001` | a \\| b \\\\| c |\n";
    let page = parse_wiki_page(markdown)?;
    assert_eq!(
        page.citations
            .first()
            .and_then(|citation| citation.note.as_deref()),
        Some("a | b \\| c")
    );
    assert_eq!(parse_wiki_page(&serialize_wiki_page(&page)?)?, page);
    Ok(())
}

#[test]
fn malformed_pages_are_typed_errors_and_slugs_cannot_name_paths() {
    for text in [
        "no frontmatter",
        "---\ntitle: x\n",
        "\n---\ntitle: x\nslug: x\n---\n",
        "--- \ntitle: x\nslug: x\n---\n",
        "---\ntitle: x\n---\n",
        "---\nslug: x\n---\n",
        "---\n\n---\n",
    ] {
        assert!(
            matches!(parse_wiki_page(text), Err(WikiPageError::Invalid(_))),
            "{text}"
        );
    }
    for text in [
        "---\ntitle: []\nslug: a\n---\n",
        "---\ntitle: x\nslug: x\naliases: one\n---\n",
        "---\ntitle: x\nslug: x\ntitle: y\n---\n",
    ] {
        assert!(
            matches!(parse_wiki_page(text), Err(WikiPageError::Yaml(_))),
            "{text}"
        );
    }
    for (slug, normalized) in [
        ("../x", "x"),
        ("a/../../b", "a-b"),
        ("/etc/passwd", "etc-passwd"),
        (".", ""),
    ] {
        let page = parse_wiki_page(&format!("---\ntitle: x\nslug: \"{slug}\"\n---\n"));
        match page {
            Ok(page) => assert_eq!(page.meta.slug.as_ref(), normalized),
            Err(error) => {
                assert_eq!(normalized, "", "{slug}");
                assert!(matches!(error, WikiPageError::Invalid(_)));
            }
        }
    }
}

#[test]
fn top_down_citation_kinds_survive_a_wiki_page_round_trip() -> TestResult {
    let markdown = "---
title: Spring inspection
slug: spring
---

## Citations

| Date | Kind | Summary | Note |
|------|------|---------|------|
| 2026-10-10 | session | `00000000-0000-4000-8000-000000000001` | Inspection |
| 2026-10-10 | part | `00000000-0000-4000-8000-000000000002` | Measurements |
| 2026-10-10 | section | `00000000-0000-4000-8000-000000000003` | Tension |
";
    let page = parse_wiki_page(markdown)?;
    assert_eq!(
        page.citations
            .iter()
            .filter_map(|citation| citation.kind)
            .map(|kind| kind.to_string())
            .collect::<Vec<_>>(),
        ["session", "part", "section"]
    );
    assert_eq!(parse_wiki_page(&serialize_wiki_page(&page)?)?, page);
    Ok(())
}

#[test]
fn free_text_citation_reads_new_kinds_only_after_a_date() -> TestResult {
    let markdown = "---
title: Spring inspection
slug: spring
---

## Citations

- `00000000-0000-4000-8000-000000000001` the session where we measured part of the spring
- `00000000-0000-4000-8000-000000000002` 2026-10-10 we measured part of the spring
- `00000000-0000-4000-8000-000000000003` measured on 2026-10-10 section of the spring
- `00000000-0000-4000-8000-000000000004` BRANCH noted in the session
";
    let page = parse_wiki_page(markdown)?;
    let kinds: Vec<_> = page
        .citations
        .iter()
        .map(|citation| citation.kind.map(|kind| kind.to_string()))
        .collect();
    assert_eq!(
        kinds,
        [
            None,
            None,
            Some("section".to_owned()),
            Some("branch".to_owned())
        ]
    );
    assert!(
        serialize_wiki_page(&page)?
            .contains("|  | leaf | `00000000-0000-4000-8000-000000000001` |")
    );
    Ok(())
}

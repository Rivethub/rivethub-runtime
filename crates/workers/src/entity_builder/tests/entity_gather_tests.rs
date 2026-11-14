use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::TranscriptMessage;
use kernel::wiki::entity_pass::normalize_evidence;
use uuid::Uuid;

use super::passage_report;
use super::passages;
use super::passages_within;
use crate::entity_builder::transcript::render_message;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |time| time.with_timezone(&Utc))
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
struct Tool<'a> {
    name: &'a str,
    args: Option<serde_json::Value>,
    result: Option<&'a str>,
}

fn said(id: u128, stamp: &str, content: &str) -> TranscriptMessage {
    line(id, stamp, "user", content, None)
}

fn line(
    id: u128,
    stamp: &str,
    role: &str,
    content: &str,
    tool: Option<&Tool<'_>>,
) -> TranscriptMessage {
    TranscriptMessage {
        id: Uuid::from_u128(id),
        at: at(stamp),
        role: role.to_owned(),
        tool_name: tool.as_ref().map(|tool| tool.name.to_owned()),
        content: content.to_owned(),
        tool_args: tool.as_ref().and_then(|tool| tool.args.clone()),
        tool_result: tool
            .as_ref()
            .and_then(|tool| tool.result.map(str::to_owned)),
    }
}

#[test]
fn an_alias_matches_on_a_word_boundary() {
    let messages = vec![
        said(1, "2026-11-07T12:00:00Z", "GPUBox is up"),
        said(2, "2026-11-07T12:01:00Z", "the gpuboxes rack"),
    ];
    let text = passages(&messages, "gpubox", &[], "");
    assert!(text.contains("GPUBox is up"));
    assert!(!text.contains("gpuboxes"));
}

#[test]
fn a_matched_call_keeps_its_result_and_a_matched_result_keeps_its_call() {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    let hostname = serde_json::json!({"command": "hostname"});
    let call_only = vec![
        line(
            1,
            "2026-11-07T12:00:00Z",
            "assistant",
            "check gpubox",
            Some(&Tool {
                name: "Bash",
                args: Some(hostname.clone()),
                result: None,
            }),
        ),
        line(
            2,
            "2026-11-07T12:00:01Z",
            "tool",
            "",
            Some(&Tool {
                name: "Bash",
                args: None,
                result: Some("ok\n"),
            }),
        ),
    ];
    let from_call = passages(&call_only, "gpubox", &[], "");
    assert!(from_call.contains("check gpubox"));
    assert!(from_call.contains("ok"));
    let result_only = vec![
        line(
            3,
            "2026-11-07T12:02:00Z",
            "assistant",
            "hostname",
            Some(&Tool {
                name: "Bash",
                args: Some(hostname),
                result: None,
            }),
        ),
        line(
            4,
            "2026-11-07T12:02:01Z",
            "tool",
            "",
            Some(&Tool {
                name: "Bash",
                args: None,
                result: Some("gpubox\n"),
            }),
        ),
    ];
    let from_result = passages(&result_only, "gpubox", &[], "");
    assert!(from_result.contains("hostname"));
    assert!(from_result.contains("gpubox"));
}

#[test]
fn the_cap_keeps_the_latest_passages() {
    let messages = vec![
        said(1, "2026-11-07T12:00:00Z", "early-token gpubox"),
        said(2, "2026-11-07T12:01:00Z", "mid-token gpubox"),
        said(3, "2026-11-07T12:02:00Z", "latest-token gpubox"),
    ];
    let latest_only = vec![said(3, "2026-11-07T12:02:00Z", "latest-token gpubox")];
    let latest = passages_within(&latest_only, "gpubox", &[], "", usize::MAX);
    let text = passages_within(&messages, "gpubox", &[], "", latest.chars().count());
    assert!(text.contains("latest-token"));
    assert!(!text.contains("early-token"));
    assert!(!text.contains("mid-token"));
}

#[test]
fn a_mention_matches_when_the_name_never_appears() {
    let quote = "agent CTs 113/116/117/118/112/107";
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    let hostname = serde_json::json!({"command": "hostname"});
    let messages = vec![
        line(
            1,
            "2026-11-07T12:00:00Z",
            "assistant",
            "read the plan",
            Some(&Tool {
                name: "Read",
                args: Some(hostname),
                result: None,
            }),
        ),
        line(
            2,
            "2026-11-07T12:00:01Z",
            "tool",
            "",
            Some(&Tool {
                name: "Read",
                args: None,
                result: Some("guests: agent CTs 113/116/117/118/112/107\n"),
            }),
        ),
    ];
    let text = passages(&messages, "node_a", &[], quote);
    assert!(text.contains(quote));
    assert!(text.contains("read the plan"));
    assert_eq!(passages(&messages, "node_a", &[], ""), "");
}

#[test]
fn a_mention_matches_through_markdown_and_whitespace() {
    let messages = vec![said(
        1,
        "2026-11-07T12:00:00Z",
        "plan:\n\nagent **CTs**\n113/116/117/118/112/107",
    )];
    let text = passages(
        &messages,
        "node_a",
        &[],
        "agent CTs 113/116/117/118/112/107",
    );
    assert!(text.contains("**CTs**"));
    assert_eq!(passages(&messages, "node_a", &[], ""), "");
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn bash(
    id: u128,
    stamp: &str,
    role: &str,
    command: Option<&str>,
    result: Option<&str>,
) -> TranscriptMessage {
    line(
        id,
        stamp,
        role,
        "",
        Some(&Tool {
            name: "Bash",
            args: command.map(|command| serde_json::json!({ "command": command })),
            result,
        }),
    )
}

fn wide_table(marks: &[(usize, &str)], rows: usize) -> String {
    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let line = marks
            .iter()
            .find_map(|(at, text)| (*at == index).then_some(*text))
            .map_or_else(
                || format!("row-{index:03} idle {}", "p".repeat(40)),
                |text| format!("{text} {}", "p".repeat(30)),
            );
        lines.push(line);
    }
    lines.join("\n")
}

#[test]
fn a_long_tool_result_keeps_the_header_and_a_window_around_the_row() {
    let table = wide_table(
        &[
            (0, "FAR-HEAD"),
            (60, "gpubox vm 101"),
            (70, "NEAR-ROW"),
            (99, "FAR-TAIL"),
        ],
        100,
    );
    let call = bash(
        1,
        "2026-11-07T12:00:00Z",
        "assistant",
        Some("pct list"),
        None,
    );
    let result = bash(2, "2026-11-07T12:00:01Z", "tool", None, Some(&table));
    let rendered = render_message(&result);
    assert!(rendered.contains("FAR-HEAD"));
    assert!(rendered.chars().count() > super::EXCERPT_CHARS);
    let before = passage_report();
    let text = passages(&[call, result], "gpubox", &[], "");
    let after = passage_report();
    assert!(text.contains("Bash pct list"));
    assert!(text.contains("[2026-11-07T12:00:01.000Z] tool (Bash)"));
    assert!(text.contains("gpubox vm 101"));
    assert!(text.contains("NEAR-ROW"));
    assert!(text.contains("lines elided"));
    assert!(!text.contains("FAR-HEAD"));
    assert!(!text.contains("FAR-TAIL"));
    let sent = u64::try_from(text.chars().count()).unwrap_or(0);
    assert!(after.passage_chars.saturating_sub(before.passage_chars) >= sent);
    assert!(after.excerpted_messages > before.excerpted_messages);
    let excerpt = after.excerpt_chars.saturating_sub(before.excerpt_chars);
    let source = after
        .excerpt_source_chars
        .saturating_sub(before.excerpt_source_chars);
    assert!(excerpt < source);
}

#[test]
fn overlapping_windows_merge_into_one_span() {
    let table = wide_table(
        &[
            (0, "FAR-HEAD"),
            (40, "gpubox here"),
            (46, "BETWEEN-HIT"),
            (52, "gpu-box here"),
            (99, "FAR-TAIL"),
        ],
        100,
    );
    let message = bash(
        1,
        "2026-11-07T12:00:00Z",
        "tool",
        Some("pct list"),
        Some(&table),
    );
    let text = passages(
        std::slice::from_ref(&message),
        "gpubox",
        &["gpu-box".to_owned()],
        "",
    );
    assert!(text.contains("Bash pct list"));
    assert!(text.contains("gpubox here"));
    assert!(text.contains("gpu-box here"));
    assert!(text.contains("BETWEEN-HIT"));
    assert!(!text.contains("FAR-HEAD"));
    assert!(!text.contains("FAR-TAIL"));
    assert_eq!(text.matches("lines elided").count(), 2);
    let start = text.find("gpubox here").unwrap_or(0);
    let end = text.find("gpu-box here").unwrap_or(0);
    let between = text.get(start..end).unwrap_or("");
    assert!(!between.contains("elided"));
}

#[test]
fn a_short_message_is_unchanged() {
    let body = format!("gpubox {}", "y".repeat(2_000));
    let message = said(1, "2026-11-07T12:00:00Z", &body);
    let text = passages(std::slice::from_ref(&message), "gpubox", &[], "");
    assert_eq!(text, render_message(&message));
    assert!(!text.contains("elided"));
}

#[test]
fn a_long_single_line_uses_a_character_window() {
    let body = format!(
        "HEADTOKEN{} gpubox {}TAILTOKEN",
        "a".repeat(3_500),
        "b".repeat(3_500)
    );
    let message = bash(
        1,
        "2026-11-07T12:00:00Z",
        "tool",
        Some("cat dump.json"),
        Some(&body),
    );
    let rendered = render_message(&message);
    assert!(rendered.contains("HEADTOKEN"));
    assert!(rendered.contains("TAILTOKEN"));
    let text = passages(std::slice::from_ref(&message), "gpubox", &[], "");
    assert!(text.contains("Bash cat dump.json"));
    assert!(text.contains("gpubox"));
    assert!(text.contains("chars elided"));
    assert!(!text.contains("HEADTOKEN"));
    assert!(!text.contains("TAILTOKEN"));
    assert!(text.chars().count() < rendered.chars().count());
}

#[test]
fn a_message_is_kept_whole_when_windows_cover_most_of_it() {
    let body = (0..100)
        .map(|index| format!("gpubox row-{index:03} {}", "x".repeat(40)))
        .collect::<Vec<_>>()
        .join("\n");
    let message = bash(
        1,
        "2026-11-07T12:00:00Z",
        "tool",
        Some("pct list"),
        Some(&body),
    );
    let rendered = render_message(&message);
    assert!(rendered.chars().count() > super::EXCERPT_CHARS);
    let text = passages(std::slice::from_ref(&message), "gpubox", &[], "");
    assert_eq!(text, rendered);
    assert!(!text.contains("elided"));
}

#[test]
fn the_excerpt_map_matches_evidence_normalization() {
    for sample in [
        "plain gpubox",
        "agent **CTs**\n113/116",
        "a_b",
        "*x*",
        "see [[wiki]](url)",
    ] {
        let chars: Vec<char> = sample.chars().collect();
        let (normalized, map) = super::normalize_map(&chars);
        assert_eq!(normalized, normalize_evidence(sample));
        assert_eq!(map.len(), normalized.chars().count());
    }
}

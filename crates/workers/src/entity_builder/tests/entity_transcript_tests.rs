use super::CARRIER_TOOLS;
use super::LOOKUP_TOOLS;
use super::MEMORY_LOOKUP_OMITTED;
use super::PAYLOAD_KEYS;
use super::SHELL_TOOLS;
use super::TOOL_RESULT_CAP;
use super::cap_tool_result;
use super::chunk_messages;
use super::clear_unknown_refs;
use super::is_memory_lookup;
use super::remember;
use super::render_message;
use super::render_preface;
use super::render_transcript;
use chrono::DateTime;
use chrono::Utc;
use kernel::queue::JobKeyMode;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityObservation;
use kernel::wiki::entity_pass::ExtractEntities;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::KnownFact;
use kernel::wiki::entity_pass::ObservedFact;
use kernel::wiki::entity_pass::TranscriptMessage;
use serde_json::json;
use uuid::Uuid;

use crate::entity_builder::ENTITY_OBSERVE_SYSTEM_PROMPT;
use crate::entity_builder::ENTITY_REWRITE_SYSTEM_PROMPT;
use crate::entity_builder::extract_entities_job;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn message(id: u128, secs: i64, role: &str, content: &str) -> TranscriptMessage {
    TranscriptMessage {
        id: uid(id),
        at: at(secs),
        role: role.to_owned(),
        tool_name: None,
        content: content.to_owned(),
        tool_args: None,
        tool_result: None,
    }
}

#[test]
fn messages_render_in_time_order_with_their_own_timestamps() {
    let later = message(1, 100, "assistant", "done");
    let earlier = message(2, 0, "user", "keep me ");
    let same = message(3, 0, "user", "second");
    let rendered = render_transcript(&[later, same, earlier]);
    let first = rendered.find("keep me ").unwrap_or(usize::MAX);
    let second = rendered.find("second").unwrap_or(0);
    let third = rendered.find("done").unwrap_or(0);
    assert!(first < second && second < third);
    assert!(rendered.contains("[1970-01-01T00:00:00.000Z] user\nkeep me "));
    assert!(rendered.contains("[1970-01-01T00:01:40.000Z] assistant\ndone"));
}

#[test]
#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn shell_calls_keep_the_command_and_cap_the_result() {
    let head = "h".repeat(TOOL_RESULT_CAP - 500);
    let tail = "t".repeat(900);
    let result = format!("{head}{tail}");
    let mut tool = message(4, 10, "tool", "[tool call] Bash");
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(json!({"command": "hostname", "description": "noise"}));
    tool.tool_result = Some(result);
    let rendered = render_message(&tool);
    assert!(rendered.starts_with("[1970-01-01T00:00:10.000Z] tool (Bash)\n"));
    assert!(rendered.contains("Bash hostname"));
    assert!(!rendered.contains("description"));
    assert!(!rendered.contains("[tool call]"));
    let capped = cap_tool_result(tool.tool_result.as_deref().unwrap_or(""));
    assert!(capped.chars().count() <= TOOL_RESULT_CAP);
    assert!(capped.ends_with(&"t".repeat(800)));
    assert!(capped.starts_with('h'));
    let original = tool.tool_result.as_deref().unwrap_or("").chars().count();
    let tail_text: String = "t".repeat(800);
    let without_tail: String = capped
        .chars()
        .take(capped.chars().count().saturating_sub(800))
        .collect();
    let Some((prefix, _)) = without_tail.split_once("[...") else {
        assert!(capped.contains("chars elided"));
        return;
    };
    let elided = original
        .saturating_sub(prefix.chars().count())
        .saturating_sub(800);
    assert!(capped.contains(&format!("[... {elided} chars elided ...]")));
    assert!(capped.ends_with(&tail_text));
    assert_eq!(cap_tool_result("short"), "short");
    assert!(!cap_tool_result(&"x".repeat(TOOL_RESULT_CAP)).contains("elided"));
    let wide = "é".repeat(TOOL_RESULT_CAP.saturating_add(1));
    let wide_capped = cap_tool_result(&wide);
    assert!(wide_capped.chars().count() <= TOOL_RESULT_CAP);
    assert!(wide_capped.ends_with(&"é".repeat(800)));
}

#[test]
#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn a_non_shell_call_keeps_compact_args() {
    let mut tool = message(5, 11, "tool", "Read");
    tool.tool_name = Some("Read".to_owned());
    tool.tool_args = Some(json!({"path": "/etc/hosts"}));
    tool.tool_result = Some("127.0.0.1 localhost\n".to_owned());
    let rendered = render_message(&tool);
    assert!(rendered.contains("[1970-01-01T00:00:11.000Z] tool (Read)"));
    assert!(rendered.contains("Read {\"path\":\"/etc/hosts\"}"));
    assert!(rendered.contains("127.0.0.1 localhost"));
    assert!(!rendered.contains("\nRead\n"));
}

#[test]
fn chunks_are_consecutive_and_a_long_message_stands_alone() {
    let one = message(1, 10, "user", &"a".repeat(30));
    let two = message(2, 20, "user", &"b".repeat(30));
    let three = message(3, 30, "user", &"c".repeat(30));
    let len_one = render_message(&one).chars().count();
    let len_two = render_message(&two).chars().count();
    let fit_two = len_one.saturating_add(2).saturating_add(len_two);
    let together = chunk_messages(&[three, one.clone(), two.clone()], fit_two);
    assert_eq!(together.len(), 2);
    let first = render_transcript(together.first().unwrap_or(&Vec::new()));
    let second = render_transcript(together.get(1).unwrap_or(&Vec::new()));
    assert!(first.contains("[1970-01-01T00:00:10.000Z]"));
    assert!(first.contains("[1970-01-01T00:00:20.000Z]"));
    assert!(!first.contains("[1970-01-01T00:00:30.000Z]"));
    assert!(second.starts_with("[1970-01-01T00:00:30.000Z]"));
    assert!(first.chars().count() <= fit_two);
    let tight = chunk_messages(&[one.clone(), two], fit_two.saturating_sub(1));
    assert_eq!(tight.len(), 2);
    let huge = message(9, 40, "user", &"z".repeat(5_000));
    let alone = chunk_messages(&[huge, one], 100);
    assert_eq!(alone.len(), 2);
    let sizes: Vec<usize> = alone
        .iter()
        .map(|chunk| render_transcript(chunk).chars().count())
        .collect();
    assert!(sizes.iter().any(|size| *size > 100));
    assert!(sizes.iter().any(|size| *size <= 100));
}

#[test]
fn preface_is_labelled_and_later_facts_replace_known_ones() {
    let leaf = EntityLeaf {
        id: uid(8),
        conversation_id: Some(uid(9)),
        content: "intent only".to_owned(),
        start: at(10),
        end: at(20),
        owner: Some("owner".to_owned()),
    };
    let preface = render_preface(std::slice::from_ref(&leaf));
    assert!(preface.contains("intent only"));
    assert!(preface.contains(&uid(8).to_string()));
    assert!(preface.contains("facts come from the transcript"));
    let mut known = vec![KnownEntity {
        id: uid(1),
        name: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        aliases: Vec::new(),
        facts: vec![KnownFact {
            attribute: "port".to_owned(),
            value: "22".to_owned(),
        }],
    }];
    let update = EntityObservation {
        name: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        facts: vec![ObservedFact {
            attribute: "port".to_owned(),
            value: "443".to_owned(),
            as_of: "1970-01-01T00:00:20.000Z".to_owned(),
            ..ObservedFact::default()
        }],
        ..EntityObservation::default()
    };
    let added = EntityObservation {
        name: "phil".to_owned(),
        type_name: "People".to_owned(),
        entity_ref: None,
        ..EntityObservation::default()
    };
    remember(&mut known, &[update, added]);
    let port = known
        .first()
        .and_then(|item| item.facts.first())
        .map(|fact| fact.value.as_str());
    assert_eq!(port, Some("443"));
    assert_eq!(known.get(1).map(|item| item.name.as_str()), Some("phil"));
    let provisional = known.get(1).map(|item| item.id);
    let mut observations = vec![EntityObservation {
        name: "phil".to_owned(),
        entity_ref: provisional,
        ..EntityObservation::default()
    }];
    observations.push(EntityObservation {
        name: "gpubox".to_owned(),
        entity_ref: Some(uid(1)),
        ..EntityObservation::default()
    });
    clear_unknown_refs(
        &mut observations,
        std::slice::from_ref(&KnownEntity {
            id: uid(1),
            name: "gpubox".to_owned(),
            type_name: "Hosts".to_owned(),
            aliases: Vec::new(),
            facts: Vec::new(),
        }),
    );
    assert_eq!(observations.first().and_then(|item| item.entity_ref), None);
    assert_eq!(
        observations.get(1).and_then(|item| item.entity_ref),
        Some(uid(1))
    );
}

#[test]
fn a_mismatched_ref_does_not_become_an_alias_of_the_known_entity() {
    let mut hosts = vec![KnownEntity {
        id: uid(1),
        name: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
    }];
    remember(
        &mut hosts,
        &[EntityObservation {
            name: "gerty".to_owned(),
            type_name: "Hosts".to_owned(),
            entity_ref: Some(uid(1)),
            aliases: vec!["g9".to_owned()],
            ..EntityObservation::default()
        }],
    );
    assert_eq!(hosts.first().map(|item| item.aliases.len()), Some(0));
    assert_eq!(hosts.get(1).map(|item| item.name.as_str()), Some("gerty"));
}

#[test]
fn observe_prompt_uses_message_time_and_the_job_key_stays_the_leaf() {
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("Identify every durable"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("Do not return facts"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("copied exactly as written"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("agent CTs 113/116/117/118/112/107"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("never a normalized name"));
    let schema = serde_json::to_string(&kernel::wiki::entity_pass::identify_schema()).unwrap();
    assert!(schema.contains("mention"));
    assert!(!ENTITY_OBSERVE_SYSTEM_PROMPT.contains("period_start"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("those are the preferred choices"));
    assert!(ENTITY_OBSERVE_SYSTEM_PROMPT.contains("type is a new short name for the kind"));
    assert!(!ENTITY_OBSERVE_SYSTEM_PROMPT.contains("exactly one of the provided type paths"));
    assert!(
        ENTITY_REWRITE_SYSTEM_PROMPT
            .contains("Tool results are the strongest evidence of current state")
    );
    assert!(
        ENTITY_REWRITE_SYSTEM_PROMPT
            .contains("A later message overrides an earlier one in the same window")
    );
    assert!(
        ENTITY_REWRITE_SYSTEM_PROMPT
            .contains("as_of must be the RFC3339 timestamp copied from the header of the message")
    );
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("vocabulary"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("87 GB"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("152 GB"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains(
        "When a value changed during the transcript, emit only the final value under the property's key"
    ));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("reported history, not observations"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("\"memory says\""));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("\"the wiki says\""));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("\"according to the notes\""));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("\"last time\""));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("never to set a current fact"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("what this session's tool output shows"));
    assert!(ENTITY_REWRITE_SYSTEM_PROMPT.contains("what the user states as fact now"));
    let summary = uid(42);
    let built = extract_entities_job(&ExtractEntities {
        summary_id: summary,
        conversation_id: Some(uid(7)),
        session_at: None,
    });
    assert!(matches!(
        built,
        Ok(job) if job.key == Some(format!("entity-ext-{summary}"))
            && job.key_mode == JobKeyMode::KeepRunAt
            && job.run_at.is_none()
    ));
    let session_at = at(1_700_000_000);
    let stamped = extract_entities_job(&ExtractEntities {
        summary_id: summary,
        conversation_id: None,
        session_at: Some(session_at),
    })
    .expect("stamped job");
    assert_eq!(stamped.run_at, Some(session_at));
}

struct LookupCase {
    label: &'static str,
    name: &'static str,
    args: &'static str,
    result: &'static str,
    lookup: bool,
}

fn sorted(names: &[&str]) -> bool {
    let mut previous = "";
    for name in names {
        if *name <= previous {
            return false;
        }
        previous = name;
    }
    true
}

#[test]
fn lookup_name_lists_stay_sorted() {
    assert!(sorted(LOOKUP_TOOLS));
    assert!(sorted(CARRIER_TOOLS));
    assert!(sorted(SHELL_TOOLS));
    assert!(sorted(PAYLOAD_KEYS));
}

const LOOKUP_CASES: &[LookupCase] = &[
    LookupCase {
        label: "cat of a wiki page",
        name: "Bash",
        args: r#"{"command":"cat /rivet-shared/wiki/topics/gpubox.md"}"#,
        result: "seven working V100s and one dead module\n",
        lookup: true,
    },
    LookupCase {
        label: "nvidia-smi",
        name: "Bash",
        args: r#"{"command":"nvidia-smi --query-gpu=index,memory.used,utilization.gpu --format=csv"}"#,
        result: "0, 100 MiB, 0 %\n",
        lookup: false,
    },
    LookupCase {
        label: "cat of fstab",
        name: "Bash",
        args: r#"{"command":"cat /etc/fstab"}"#,
        result: "UUID=abc / ext4 defaults 0 1\n",
        lookup: false,
    },
    LookupCase {
        label: "curl of the memory api",
        name: "bash",
        args: "curl -sk https://127.0.0.1:5174/api/memory/search?q=gpubox",
        result: r#"{"hits":[{"text":"RAM 51 GB used"}]}"#,
        lookup: true,
    },
    LookupCase {
        label: "curl of the wiki api",
        name: "run_terminal_command",
        args: r#"{"command":"curl -sk https://127.0.0.1:5174/api/wiki/topics/gpubox"}"#,
        result: "page",
        lookup: true,
    },
    LookupCase {
        label: "direct memory_search",
        name: "mcp__rivetos__memory_search",
        args: r#"{"query":"gpubox"}"#,
        result: "RAM 51 GB used after DS41 shutdown",
        lookup: true,
    },
    LookupCase {
        label: "direct wiki_read",
        name: "mcp__rivetos__wiki_read",
        args: r#"{"slug":"gpubox"}"#,
        result: "seven working V100s",
        lookup: true,
    },
    LookupCase {
        label: "plugin memory_browse",
        name: "mcp__plugin_rivet-memory_rivetos__memory_browse",
        args: "{}",
        result: "october notes",
        lookup: true,
    },
    LookupCase {
        label: "use_tool memory_search",
        name: "use_tool",
        args: r#"{"tool_name":"rivetos__memory_search","tool_input":{"query":"gpubox"}}"#,
        result: "stale",
        lookup: true,
    },
    LookupCase {
        label: "use_tool slash wiki_read",
        name: "use_tool",
        args: r#"{"tool_name":"rivetos/wiki_read","tool_input":{"slug":"gpubox"}}"#,
        result: "stale page",
        lookup: true,
    },
    LookupCase {
        label: "use_tool memory_stats",
        name: "use_tool",
        args: r#"{"tool_name":"rivetos__memory_stats","tool_input":{}}"#,
        result: "rows",
        lookup: true,
    },
    LookupCase {
        label: "search_tool names memory_search",
        name: "search_tool",
        args: r#"{"query":"memory_search memory_browse rivetos"}"#,
        result: "tool schemas",
        lookup: true,
    },
    LookupCase {
        label: "read of a project memory file",
        name: "read_file",
        args: r#"{"target_file":"/home/phil/.claude/projects/-home-phil/memory/ds41-slot-budget.md"}"#,
        result: "october budget",
        lookup: true,
    },
    LookupCase {
        label: "MEMORY.md",
        name: "Read",
        args: r#"{"file_path":"/repo/MEMORY.md"}"#,
        result: "notes",
        lookup: true,
    },
    LookupCase {
        label: "grep of a project memory directory",
        name: "grep",
        args: r#"{"path":"/home/phil/.claude/projects/-home-phil/memory","pattern":"DS41"}"#,
        result: "slot budget",
        lookup: true,
    },
    LookupCase {
        label: "session jsonl under claude projects is not a memory file",
        name: "Read",
        args: r#"{"file_path":"/home/phil/.claude/projects/-home-phil/session.jsonl"}"#,
        result: "transcript",
        lookup: false,
    },
    LookupCase {
        label: "read of hosts",
        name: "Read",
        args: r#"{"file_path":"/etc/hosts"}"#,
        result: "127.0.0.1 localhost\n",
        lookup: false,
    },
    LookupCase {
        label: "memory-researcher agent",
        name: "Agent",
        args: r#"{"subagent_type":"memory-researcher","prompt":"find gpubox"}"#,
        result: "launched",
        lookup: true,
    },
    LookupCase {
        label: "memory-researcher task",
        name: "Task",
        args: r#"{"description":"memory-researcher"}"#,
        result: "done",
        lookup: true,
    },
    LookupCase {
        label: "agent that is not a memory researcher",
        name: "Agent",
        args: r#"{"prompt":"map the pipeline"}"#,
        result: "launched",
        lookup: false,
    },
    LookupCase {
        label: "filePath header names the wiki page",
        name: "Read",
        args: "{}",
        result: r#"{"type":"text","file":{"filePath":"/rivet-shared/wiki/topics/gpubox.md","content":"seven working V100s"}}"#,
        lookup: true,
    },
    LookupCase {
        label: "stdout that mentions the wiki route is source, not a lookup",
        name: "Bash",
        args: r#"{"command":"nl -ba crates/gateway/src/routes/wiki.rs"}"#,
        result: r#"{"stdout":"59: .route(\"/api/wiki\", get(topics))","stderr":""}"#,
        lookup: false,
    },
    LookupCase {
        label: "file content mentions a wiki path but the path read is not one",
        name: "Read",
        args: r#"{"file_path":"/etc/hostname"}"#,
        result: r#"{"type":"text","file":{"filePath":"/etc/hostname","content":"see MEMORY.md and /rivet-shared/wiki/topics/gpubox.md"}}"#,
        lookup: false,
    },
    LookupCase {
        label: "plain result header is the wiki path",
        name: "bash",
        args: "",
        result: "/rivet-shared/wiki/topics/gpubox.md\nseven working V100s\n",
        lookup: true,
    },
    LookupCase {
        label: "wiki path on a later line is not the header",
        name: "Bash",
        args: "nvidia-smi",
        result: "GPU 0\n/rivet-shared/wiki/topics/gpubox.md\n",
        lookup: false,
    },
    LookupCase {
        label: "cat tool of a wiki page",
        name: "cat",
        args: "/rivet-shared/wiki/topics/gpubox.md",
        result: "page",
        lookup: true,
    },
];

#[test]
fn memory_lookup_matches_the_case_table() {
    for case in LOOKUP_CASES {
        assert_eq!(
            is_memory_lookup(case.name, case.args, case.result),
            case.lookup,
            "{}",
            case.label
        );
    }
}

#[test]
#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        untyped,
        reason = "tool arguments and model JSON are arbitrary objects"
    )
)]
fn a_rendered_transcript_keeps_the_lookup_call_and_drops_the_result() {
    let user = message(1, 0, "user", "how many gpus");
    let mut tool = message(2, 10, "tool", "[tool call] Bash");
    tool.tool_name = Some("Bash".to_owned());
    tool.tool_args = Some(json!({"command": "cat /rivet-shared/wiki/topics/gpubox.md"}));
    tool.tool_result = Some("seven working V100s and one dead module\n".to_owned());
    let rendered = render_transcript(&[tool, user]);
    assert!(rendered.contains("how many gpus"));
    assert!(rendered.contains("Bash cat /rivet-shared/wiki/topics/gpubox.md"));
    assert!(rendered.contains(MEMORY_LOOKUP_OMITTED));
    assert!(!rendered.contains("seven working V100s"));
    assert!(!rendered.contains("dead module"));
    assert!(!rendered.contains("[tool call]"));
}

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use futures::executor::block_on;
use kernel::ToolContext;
use kernel::harness::ApprovalDecision as Decision;
use kernel::harness::Command;
use kernel::harness::HarnessAdapter;
use kernel::harness::PlanEntry;
use kernel::harness::PlanPriority;
use kernel::harness::PlanStatus;
use kernel::harness::SessionEvent;
use kernel::harness::SessionOption;
use kernel::harness::SessionSpec;
use kernel::harness::ToolCallInfo;
use kernel::harness::ToolCallStatus;
use kernel::harness::ToolCallUpdate;
use kernel::harness::TurnInput;
use kernel::memory::BrowseFilter;
use kernel::memory::BrowseWindow;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagEntryPair;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use kernel::model::ModelChunk;
use kernel::model::ModelProvider;
use kernel::model::ModelRequest;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::WorkerRole;
use kernel::tool::Tool;
use kernel::transcript::TranscriptReader;
use kernel::transcript::Turn;
use testkit::FakeHarness;
use testkit::FakeJobQueue;
use testkit::FakeMemoryStore;
use testkit::FakeProvider;
use testkit::FakeReader;
use testkit::FakeTool;
use testkit::MemoryRequest;
use testkit::Script;
use uuid::Uuid;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

fn memory_entry(session: &str, content: &str, at: DateTime<Utc>) -> MemoryEntry {
    MemoryEntry {
        provenance: None,
        id: Some(format!("m-{}", content.len())),
        session_id: session.to_string(),
        agent: "agent-a".to_string(),
        channel: "test".to_string(),
        role: Role::User,
        content: content.to_string(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at),
    }
}

fn search_query(text: &str) -> SearchQuery {
    SearchQuery {
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
        text: text.to_string(),
        mode: SearchMode::Hybrid,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(None, 10, 1..=50),
    }
}

fn browse_filter() -> BrowseFilter {
    BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: true,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(None, 50, 1..=200),
    }
}

#[test]
fn memory_store_roundtrip() {
    block_on(async {
        let store: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
        let now = Utc::now();
        let inserted = store
            .append(
                &testkit::owner().unwrap(),
                vec![memory_entry("claude-code:abc", "hello world", now)],
            )
            .await
            .unwrap();
        assert_eq!(inserted, 1);
        let hits = store.search(search_query("hello")).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        let stats = store.stats().await.unwrap();
        assert_eq!(stats.messages, 1);
        let browsed = store.browse(browse_filter()).await.unwrap();
        assert_eq!(browsed.len(), 1);
        let full = store.get_full("m-11").await.unwrap();
        assert!(full.is_some());
        let context = store.context("claude-code:abc", now).await.unwrap();
        assert_eq!(context, "hello world");
        let changed = store
            .decide_tags(
                vec![TagId::from_str("00000000-0000-4000-8000-000000000001").unwrap()],
                TagDecision::Accept,
                None,
            )
            .await
            .unwrap();
        assert_eq!(changed, 0);
    });
}

#[test]
fn fake_memory_store_records_typed_requests() {
    block_on(async {
        let store = FakeMemoryStore::default();
        let query = SearchQuery {
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
            text: "hello".to_string(),
            mode: SearchMode::Fts,
            scope: MemoryScope::Messages,
            agent: Some("agent-a".to_string()),
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: Some("project:acme".parse().unwrap()),
            limit: Limit::clamped(Some(500), 20, 1..=50),
        };
        store.search(query.clone()).await.unwrap();
        let filter = BrowseFilter {
            conversation_id: Some(Uuid::parse_str("00000000-0000-4000-8000-000000000009").unwrap()),
            agent: Some("agent-a".to_string()),
            role: Some(Role::User),
            tool_name: Some("shell".to_string()),
            tag: Some("project:acme".parse().unwrap()),
            window: Some(BrowseWindow::Last7d),
            since: None,
            before: None,
            include_tools: false,
            order: MemoryBrowseOrder::Asc,
            limit: Limit::clamped(Some(200), 50, 1..=200),
        };
        store.browse(filter.clone()).await.unwrap();
        let requests = store.requests().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests.first(), Some(&MemoryRequest::Search(query)));
        assert_eq!(requests.get(1), Some(&MemoryRequest::Browse(filter)));
    });
}

#[test]
fn fake_memory_store_records_tag_read_requests() {
    block_on(async {
        let store = FakeMemoryStore::default();
        let list = TagListQuery {
            entity_type: Some(TagEntityType::Conversation),
            entity_id: Some(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()),
            key: Some("project".to_string()),
            value: Some("acme".to_string()),
            states: vec![TagState::Suggested],
            limit: Limit::clamped(None, 200, 1..=200),
        };
        store.list_tags(list.clone()).await.unwrap();
        let limit = Limit::clamped(None, 50, 1..=200);
        store.pending_tags(limit).await.unwrap();
        let counts = TagCountsQuery {
            key: Some("project".to_string()),
            limit: Limit::clamped(None, 200, 1..=200),
        };
        store.tag_counts(counts.clone()).await.unwrap();
        let taxonomy = TagTaxonomyQuery {
            key: Some("project".to_string()),
            state: Some(TagState::Accepted),
            limit: Limit::clamped(None, 500, 1..=500),
        };
        store.list_tag_taxonomy(taxonomy.clone()).await.unwrap();
        let requests = store.requests().await;
        assert_eq!(requests.len(), 4);
        assert_eq!(requests.first(), Some(&MemoryRequest::ListTags(list)));
        assert_eq!(requests.get(1), Some(&MemoryRequest::PendingTags(limit)));
        assert_eq!(requests.get(2), Some(&MemoryRequest::TagCounts(counts)));
        assert_eq!(requests.get(3), Some(&MemoryRequest::TagTaxonomy(taxonomy)));
    });
}

#[tokio::test]
async fn fake_memory_store_records_tag_write_requests() {
    let store = FakeMemoryStore::default();
    let ids = vec![
        TagId::from_str("00000000-0000-4000-8000-000000000003").unwrap(),
        TagId::from_str("00000000-0000-4000-8000-000000000004").unwrap(),
    ];
    store
        .decide_tags(ids.clone(), TagDecision::Reject, Some("owner".to_string()))
        .await
        .unwrap();
    let add = TagAdd {
        entity_type: TagEntityType::Conversation,
        entity: TagEntity::SessionKey("claude-code:abc".to_string()),
        agent: Some("agent-a".to_string()),
        tag: "project:acme".parse().unwrap(),
        display: Some("Acme".to_string()),
        reason: None,
        decided_by: None,
    };
    store
        .script_session_entity("claude-code:abc".to_owned(), Uuid::new_v4())
        .await;
    let added = store.add_tag(add.clone()).await.unwrap();
    assert_eq!(added.key, "project");
    assert_eq!(added.value, "acme");
    assert_eq!(added.state, TagState::Accepted);
    let lookup = TagLookup {
        session_keys: vec!["claude-code:abc".to_string()],
        states: vec![TagState::Accepted],
    };
    store.lookup_tags(lookup.clone()).await.unwrap();
    let upsert = TagTaxonomyUpsert {
        key: "project".to_string(),
        value: "acme".to_string(),
        display: None,
        parent_value: Some("clients".to_string()),
        aliases: vec!["acme-corp".to_string()],
        reason: None,
    };
    let entry = store.upsert_tag_taxonomy(upsert.clone()).await.unwrap();
    assert_eq!(entry.parent_value.as_deref(), Some("clients"));
    let decision = TagTaxonomyDecision {
        entries: vec![TagEntryPair {
            key: "project".to_string(),
            value: "acme".to_string(),
        }],
        decision: TagDecision::Accept,
        decided_by: None,
    };
    store.decide_tag_taxonomy(decision.clone()).await.unwrap();
    let merge = TagTaxonomyMerge {
        key: "project".to_string(),
        from: "acme-corp".to_string(),
        into: "acme".to_string(),
    };
    let merged = store.merge_tag_taxonomy(merge.clone()).await.unwrap();
    assert_eq!(merged.into, "acme");
    let requests = store.requests().await;
    assert_eq!(requests.len(), 6);
    assert_eq!(
        requests.first(),
        Some(&MemoryRequest::DecideTags {
            ids,
            decision: TagDecision::Reject,
            decided_by: Some("owner".to_string()),
        })
    );
    assert_eq!(requests.get(1), Some(&MemoryRequest::AddTag(add)));
    assert_eq!(requests.get(2), Some(&MemoryRequest::LookupTags(lookup)));
    assert_eq!(
        requests.get(3),
        Some(&MemoryRequest::UpsertTagTaxonomy(upsert))
    );
    assert_eq!(
        requests.get(4),
        Some(&MemoryRequest::DecideTagTaxonomy(decision))
    );
    assert_eq!(
        requests.get(5),
        Some(&MemoryRequest::MergeTagTaxonomy(merge))
    );
}

#[test]
fn fake_memory_store_records_tag_removal_decisions() {
    block_on(async {
        let store = FakeMemoryStore::default();
        let ids = vec![
            TagId::from_str("00000000-0000-4000-8000-000000000005").unwrap(),
            TagId::from_str("00000000-0000-4000-8000-000000000006").unwrap(),
        ];
        let changed = store
            .decide_tag_removals(ids.clone(), TagDecision::Accept, Some("owner".to_string()))
            .await
            .unwrap();
        assert_eq!(changed, 0);
        let requests = store.requests().await;
        assert_eq!(
            requests,
            vec![MemoryRequest::DecideTagRemovals {
                ids,
                decision: TagDecision::Accept,
                decided_by: Some("owner".to_string()),
            }]
        );
    });
}

#[test]
fn job_queue_claims_in_order() {
    block_on(async {
        let queue: Arc<dyn JobQueue> = Arc::new(FakeJobQueue::default());
        queue
            .enqueue(Job {
                name: JobName::Fixed(FixedJob::EmbedTarget),
                payload: wire::JsonObject::default(),
                key: Some("task:1".to_string()),
                key_mode: JobKeyMode::Replace,
                priority: 0,
                run_at: None,
                max_attempts: None,
            })
            .await
            .unwrap();
        let claimed = queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder),
            ))
            .await
            .unwrap()
            .expect("expected one queued job");
        assert_eq!(claimed.name, JobName::Fixed(FixedJob::EmbedTarget));
        queue.complete(claimed.id).await.unwrap();
        assert!(
            queue
                .claim(&kernel::queue::ClaimSelector::matching(
                    queue.task_names(WorkerRole::Embedder)
                ))
                .await
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn provider_streams_scripted_then_refuses_unscripted_requests() {
    block_on(async {
        let provider: Arc<dyn ModelProvider> =
            Arc::new(FakeProvider::new(vec![Script::Stream(vec![
                ModelChunk::Text {
                    delta: "scripted".to_string(),
                },
                ModelChunk::Done,
            ])]));
        let request = ModelRequest {
            options: kernel::model::ModelOptions::default(),
            model: "fake-model".to_string(),
            messages: vec![wire::message::Message {
                role: Role::User,
                content: wire::message::MessageContent::Text("hi".to_string()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: Vec::new(),
            thinking: Some(wire::stream::ThinkingLevel::Medium),
            max_tokens: None,
        };
        let mut rx = provider.stream(request.clone()).await.unwrap();
        match rx.recv().await.expect("expected a chunk") {
            Ok(chunk) => assert_eq!(
                chunk,
                ModelChunk::Text {
                    delta: "scripted".to_string()
                }
            ),
            Err(error) => panic!("the scripted item is a chunk: {error}"),
        }
        match rx.recv().await {
            Some(Ok(ModelChunk::Done)) => {}
            other => panic!("the scripted stream ends with Done, got {other:?}"),
        }
        assert!(provider.stream(request.clone()).await.is_err());
        assert!(provider.chat(request).await.is_err());
        assert!(provider.embed(vec!["abc".to_string()]).await.is_err());
        assert!(provider.list_models().await.is_err());
    });
}

#[test]
fn harness_delivers_session_events() {
    block_on(async {
        let session = SessionId::from_str("grok-build:native-1").unwrap();
        let events = vec![
            SessionEvent::ToolCall(ToolCallInfo {
                tool_call_id: "t1".to_string(),
                name: "shell".to_string(),
                input: wire::JsonObject::default(),
            }),
            SessionEvent::ToolCallUpdate(ToolCallUpdate {
                tool_call_id: "t1".to_string(),
                status: ToolCallStatus::Completed,
                output: Some("done".to_string()),
                title: None,
                input: None,
            }),
            SessionEvent::Plan(vec![PlanEntry {
                content: "step".to_string(),
                priority: PlanPriority::High,
                status: PlanStatus::Pending,
            }]),
            SessionEvent::AvailableCommands(vec![Command {
                name: "compact".to_string(),
                description: "compact context".to_string(),
            }]),
            SessionEvent::TurnEnded,
        ];
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(FakeHarness::new(events));
        let created = adapter
            .new_session(SessionSpec {
                session: session.clone(),
                harness: HarnessId::GrokBuild,
                model: None,
                effort: None,
                working_dir: None,
            })
            .await
            .unwrap();
        assert_eq!(created, session);
        let mut rx = adapter.events(&session).await.unwrap();
        assert!(adapter.events(&session).await.is_err());
        adapter
            .prompt(&session, TurnInput::Text("go".to_string()))
            .await
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..5 {
            let event = rx.recv().await.expect("expected an event");
            seen.push(event);
        }
        assert_eq!(seen.len(), 5);
        adapter
            .answer_permission(&session, "r1", Decision::Allow)
            .await
            .unwrap();
        adapter
            .set_option(&session, SessionOption::Model("m".to_string()))
            .await
            .unwrap();
    });
}

#[test]
fn tool_echoes_text_argument() {
    block_on(async {
        let tool: Arc<dyn Tool> = Arc::new(FakeTool::new("echo").expect("echo tool schema"));
        let definition = tool.definition();
        assert_eq!(definition.name.as_ref(), "echo");
        let args = serde_json::json!({"text": "ping"});
        let result = tool
            .call(
                args.as_object().cloned(),
                ToolContext {
                    agent_id: Some("agent-a".to_string()),
                    working_dir: None,
                    session: None,
                },
            )
            .await
            .unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let missing = tool
            .call(
                None,
                ToolContext {
                    agent_id: None,
                    working_dir: None,
                    session: None,
                },
            )
            .await;
        assert!(missing.is_err());
    });
}

#[test]
fn reader_reads_and_tails() {
    block_on(async {
        let session = SessionId::from_str("codex:native-9").unwrap();
        let early = Turn {
            role: Role::User,
            content: "early".to_string(),
            at: DateTime::parse_from_rfc3339("2026-10-12T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let late = Turn {
            role: Role::Assistant,
            content: "late".to_string(),
            at: DateTime::parse_from_rfc3339("2026-10-12T01:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let mut turns = BTreeMap::new();
        turns.insert(session.to_string(), vec![early.clone(), late.clone()]);
        let reader: Arc<dyn TranscriptReader> =
            Arc::new(FakeReader::new(turns).expect("valid scripted sessions"));
        let listed = reader
            .sessions(std::path::Path::new("/work/acme/.claude"))
            .await
            .unwrap();
        assert_eq!(listed, vec![session.clone()]);
        let read = reader.read(&session).await.unwrap();
        assert_eq!(read.len(), 2);
        let since = DateTime::parse_from_rfc3339("2026-10-12T00:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let tail = reader.tail(&session, since).await.unwrap();
        assert_eq!(tail, vec![late]);
    });
}

#[test]
fn fake_memory_assigns_ids_and_timestamps_when_appending() {
    block_on(async {
        let store = FakeMemoryStore::default();
        let mut entry = memory_entry("claude-code:fresh", "fresh", Utc::now());
        entry.id = None;
        entry.created_at = None;
        store
            .append(&testkit::owner().unwrap(), vec![entry])
            .await
            .expect("append");
        let outcome = store.search(search_query("fresh")).await.expect("search");
        let result = outcome.results.first().expect("inserted entry");
        assert!(Uuid::parse_str(&result.id).is_ok());
        assert!(result.created_at.timestamp() > 0);
    });
}

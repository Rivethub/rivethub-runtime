use super::*;

#[test]
fn every_recorded_scenario_is_replayed_and_other_prompts_are_named() {
    let fixture = fixture();
    assert_ne!(fixture.prompts_sha256.leaf, "");
    assert_ne!(fixture.prompts_sha256.branch, "");
    assert_ne!(fixture.prompts_sha256.root, "");
    assert_ne!(fixture.prompts_sha256.wiki_extract, "");
    assert_ne!(fixture.prompts_sha256.wiki_recompile, "");
    assert_ne!(fixture.prompts_sha256.tag, "");
    let mut names: Vec<&str> = fixture
        .scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect();
    let mut replayed = REPLAYED.to_vec();
    names.sort_unstable();
    replayed.sort_unstable();
    assert_eq!(names, replayed);
    for (name, reason) in SKIPPED {
        assert_ne!(*reason, "", "{name}");
        assert!(!REPLAYED.contains(name), "{name}");
    }
    assert!(
        fixture
            .scenarios
            .iter()
            .all(|scenario| !scenario.steps.is_empty())
    );
    let mut counts = 0u64;
    let mut rows = 0usize;
    for scenario in &fixture.scenarios {
        for step in &scenario.steps {
            match &step.result {
                StepResult::Count(count) => counts = counts.saturating_add(*count),
                StepResult::Health(_) => {}
                StepResult::Rows(recorded) => rows = rows.saturating_add(recorded.len()),
            }
        }
        if let Some(ChatCalls::Count(recorded)) = &scenario.chat_calls {
            counts = counts.saturating_add(*recorded);
        }
    }
    assert!(counts > 0);
    assert!(rows > 0);
}

#[tokio::test]
async fn a_full_window_summarizes_once() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(0));
    let id = recorded
        .tables
        .summaries
        .first()
        .expect("summary")
        .conversation_id;
    let store = Arc::new(MemStore::new(vec![window(
        id,
        "cli:s1",
        "Session cli:s1",
        messages(at("2026-10-10T12:00:00Z"), 10),
    )]));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let now = at("2026-10-10T12:12:00Z");
    let env = bind(&store, &script, &jobs, &settings, now, &token);
    assert_eq!(handle_enqueue_idle(&env).await.expect("sweep"), 1);
    let job = jobs.only().await;
    assert_eq!(job.key.as_deref(), Some(format!("compact-{id}").as_str()));
    assert_eq!(job.key_mode, JobKeyMode::Arm);
    assert_eq!(job.priority, -20);
    assert_eq!(read_trigger(&job.payload), SummaryTrigger::SessionIdle);
    assert_eq!(job.max_attempts, Some(3));
    let outcome = handle_summarize(&env, &job.payload)
        .await
        .expect("summarize");
    assert_eq!(
        outcome,
        SummarizeOutcome {
            summaries: 1,
            reenqueued: false
        }
    );
    let again = handle_summarize(&env, &job.payload).await.expect("second");
    assert_eq!(again.summaries, 0);
    assert_calls(&script.calls.lock().await, &chat_calls(recorded));
    assert_rows(&store.written.lock().await, &recorded.tables.summaries);
    assert_eq!(summarized_counts(&recorded.logs), vec![1]);
}

#[tokio::test]
async fn small_batches_roll_one_branch_then_the_root_on_the_next_job() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(1));
    let id = recorded
        .tables
        .summaries
        .first()
        .expect("summary")
        .conversation_id;
    let seeded = messages(at("2026-10-10T12:00:00Z"), 20);
    let store = Arc::new(MemStore::new(vec![window(
        id,
        "cli:s1",
        "Session cli:s1",
        seeded.clone(),
    )]));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = small_settings();
    let token = cancel();
    let env = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-10T12:20:00Z"),
        &token,
    );
    let first_payload = payload_of(id, SummaryTrigger::Threshold);
    let first = handle_summarize(&env, &first_payload)
        .await
        .expect("pass 1");
    assert_eq!(
        first,
        SummarizeOutcome {
            summaries: 5,
            reenqueued: true
        }
    );
    let second_job = jobs.only().await;
    assert_eq!(read_trigger(&second_job.payload), SummaryTrigger::Threshold);
    let second = handle_summarize(&env, &second_job.payload)
        .await
        .expect("pass 2");
    assert_eq!(
        second,
        SummarizeOutcome {
            summaries: 2,
            reenqueued: false
        }
    );
    assert_eq!(jobs.jobs.lock().await.len(), 0);
    assert_calls(&script.calls.lock().await, &chat_calls(recorded));
    assert_rows(&store.written.lock().await, &recorded.tables.summaries);
    assert_sources(&store.written.lock().await, &seeded);
    assert_eq!(summarized_counts(&recorded.logs), vec![5, 2]);
    assert_eq!(
        recorded
            .steps
            .iter()
            .filter(|step| step.label.starts_with("pass"))
            .count(),
        6
    );
}

#[tokio::test]
async fn a_truncated_leaf_shrinks_the_batch_and_sticks() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(2));
    let id = recorded
        .tables
        .summaries
        .first()
        .expect("summary")
        .conversation_id;
    let seeded = messages(at("2026-10-10T12:00:00Z"), 10);
    let store = Arc::new(MemStore::new(vec![window(
        id,
        "cli:s1",
        "Session cli:s1",
        seeded.clone(),
    )]));
    let script = Arc::new(Scripted::truncate_once());
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let env = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-10T12:10:00Z"),
        &token,
    );
    let outcome = handle_summarize(&env, &payload_of(id, SummaryTrigger::Threshold))
        .await
        .expect("summarize");
    assert_eq!(
        outcome,
        SummarizeOutcome {
            summaries: 2,
            reenqueued: false
        }
    );
    assert_calls(&script.calls.lock().await, &chat_calls(recorded));
    assert_rows(&store.written.lock().await, &recorded.tables.summaries);
    assert_sources(&store.written.lock().await, &seeded);
    assert_eq!(summarized_counts(&recorded.logs), vec![2]);
}

#[tokio::test]
async fn a_truncated_rollup_takes_fewer_leaves_and_a_gone_conversation_is_dropped() {
    let id = Uuid::new_v4();
    let start = at("2026-10-28T12:00:00Z");
    let mut leaves_only = window(id, "cli:s1", "Session cli:s1", Vec::new());
    leaves_only.leaves = (0..4)
        .map(|index| SummaryNode {
            id: Uuid::new_v4(),
            kind: SummaryKind::Leaf,
            content: format!("leaf {index}: {LEAF_TEXT}"),
            message_count: 10,
            earliest_at: start,
            latest_at: start,
            depth: 0,
        })
        .collect();
    let store = Arc::new(MemStore::new(vec![leaves_only]));
    let script = Arc::new(Scripted::truncate_once());
    let jobs = Arc::new(Seen::default());
    let settings = SummarySettings::from_parts(SummaryParts {
        branch_batch: 4,
        ..small_parts()
    })
    .expect("settings");
    let token = cancel();
    let env = bind(&store, &script, &jobs, &settings, start, &token);
    let outcome = handle_summarize(&env, &payload_of(id, SummaryTrigger::SessionIdle))
        .await
        .expect("a shorter rollup");
    assert_eq!(outcome.summaries, 1);
    let kinds: Vec<String> = script
        .calls
        .lock()
        .await
        .iter()
        .map(|call| call.kind.clone())
        .collect();
    assert_eq!(kinds, ["BRANCH_SYSTEM_PROMPT", "BRANCH_SYSTEM_PROMPT"]);
    let written = store.written.lock().await;
    let branch = written.first().expect("branch");
    assert_eq!(
        (branch.kind, branch.child_ids.len()),
        (SummaryKind::Branch, 2)
    );
    drop(written);
    let gone = handle_summarize(
        &env,
        &payload_of(Uuid::new_v4(), SummaryTrigger::SessionStale),
    )
    .await
    .expect("a deleted conversation's job completes");
    assert_eq!(gone.summaries, 0);
    assert_eq!(script.calls.lock().await.len(), 2);
}

#[tokio::test]
async fn a_failing_model_fails_the_job_and_skips_heartbeats() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(3));
    assert!(matches!(recorded.chat_calls, Some(ChatCalls::Count(_))));
    let id = Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("id");
    let store = Arc::new(MemStore::new(vec![
        window(
            id,
            "cli:s1",
            "Session cli:s1",
            messages(at("2026-10-04T12:00:00Z"), 10),
        ),
        window(
            Uuid::new_v4(),
            "heartbeat:pulse",
            "heartbeat",
            messages(at("2026-10-04T12:00:00Z"), 10),
        ),
    ]));
    let script = Arc::new(Scripted::fail_with(vec![503]));
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let env = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-04T12:22:00Z"),
        &token,
    );
    let object = recorded_payload(recorded);
    assert_eq!(read_trigger(&object), SummaryTrigger::SessionIdle);
    let error = handle_summarize(&env, &object).await.expect_err("503");
    assert_eq!(error.to_string(), "LLM HTTP 503");
    let job = recorded.tables.jobs.first().expect("job");
    assert_eq!(job.last_error, "LLM HTTP 503");
    assert_eq!(job.state, "queued");
    assert_eq!(job.attempts, 1);
    assert_eq!(job.max_attempts, 3);
    assert_eq!(job.task, "compact-conversation");
    assert_eq!(store.written.lock().await.len(), 0);
    assert_eq!(script.calls.lock().await.len(), 1);
    let heartbeat = store
        .windows
        .lock()
        .await
        .iter()
        .find(|window| window.conversation.session_key.starts_with("heartbeat:"))
        .expect("heartbeat")
        .conversation
        .id;
    let skipped = handle_summarize(&env, &payload_of(heartbeat, SummaryTrigger::SessionIdle))
        .await
        .expect("heartbeat");
    assert_eq!(skipped.summaries, 0);
    assert_eq!(script.calls.lock().await.len(), 1);
    assert_eq!(store.pending().await.expect("pending").len(), 1);

    assert_backoff(id).await;
}

fn recorded_payload(recorded: &Scenario) -> wire::JsonObject {
    let payload = recorded
        .tables
        .jobs
        .first()
        .expect("job")
        .payload
        .clone()
        .expect("recorded payload");
    serde_json::from_str(&payload).expect("payload")
}

async fn assert_backoff(id: Uuid) {
    let queue = Queue::new();
    let queued = queue
        .push(
            JobName::Fixed(FixedJob::SummarizeConversation),
            payload_of(id, SummaryTrigger::SessionIdle),
            3,
        )
        .await;
    let claimed = queue
        .claim(&kernel::queue::ClaimSelector::matching(
            queue.task_names(WorkerRole::Summarizer),
        ))
        .await
        .expect("claim")
        .expect("job");
    queue.fail(claimed.id, "LLM HTTP 503").await.expect("fail");
    let view = queue.view(queued).await;
    assert_eq!(view.phase, Phase::Backoff);
    assert_eq!(view.attempt, 1);
    assert_eq!(view.error.as_deref(), Some("LLM HTTP 503"));
}

#[tokio::test]
async fn configured_token_caps_bound_the_leaf_request() {
    let id = Uuid::parse_str("00000000-0000-4000-8000-000000000003").expect("id");
    let store = Arc::new(MemStore::new(vec![window(
        id,
        "cli:s1",
        "Session cli:s1",
        messages(at("2026-10-10T12:00:00Z"), 10),
    )]));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = settings()
        .with_max_tokens(4096, 6144, 8192)
        .expect("positive caps");
    let token = cancel();
    let env = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-10T12:10:00Z"),
        &token,
    );
    handle_summarize(&env, &payload_of(id, SummaryTrigger::Threshold))
        .await
        .expect("summarize");
    let caps: Vec<_> = script
        .calls
        .lock()
        .await
        .iter()
        .filter(|call| call.kind == "LEAF_SYSTEM_PROMPT")
        .map(|call| call.max_tokens)
        .collect();
    assert_ne!(caps.len(), 0);
    assert!(caps.iter().all(|cap| *cap == Some(4096)), "{caps:?}");
    assert!(settings.clone().with_max_tokens(0, 1, 1).is_err());
}

#[tokio::test]
async fn the_first_leaf_names_a_session_whose_title_is_weak() {
    let weak = Uuid::parse_str("00000000-0000-4000-8000-000000000004").expect("id");
    let named = Uuid::parse_str("00000000-0000-4000-8000-000000000005").expect("id");
    let store = Arc::new(MemStore::new(vec![
        window(
            weak,
            "cli:weak",
            "Session cli:weak",
            messages(at("2026-10-10T12:00:00Z"), 10),
        ),
        window(
            named,
            "cli:named",
            "AcmeKit deploy plan",
            messages(at("2026-10-10T12:00:00Z"), 10),
        ),
    ]));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let env = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-10T12:10:00Z"),
        &token,
    );
    for id in [weak, named] {
        handle_summarize(&env, &payload_of(id, SummaryTrigger::Threshold))
            .await
            .expect("summarize");
    }
    let kinds: Vec<String> = script
        .calls
        .lock()
        .await
        .iter()
        .map(|call| call.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            "LEAF_SYSTEM_PROMPT",
            "TITLE_SYSTEM_PROMPT",
            "LEAF_SYSTEM_PROMPT"
        ]
    );
    assert_eq!(
        *store.retitled.lock().await,
        [(weak, "Plan blue-green AcmeKit deploys".to_owned())]
    );
}

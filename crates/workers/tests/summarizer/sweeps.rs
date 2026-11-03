use super::*;

#[tokio::test]
async fn a_permanent_status_fails_and_the_reap_flags_dead_jobs() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(4));
    let row = recorded.tables.summaries.first().expect("summary");
    let store = Arc::new(MemStore::new(vec![window(
        row.conversation_id,
        "cli:s1",
        "Session cli:s1",
        messages(row.earliest_at, 10),
    )]));
    let script = Arc::new(Scripted::fail_with(vec![400, 400, 400]));
    let jobs = Arc::new(Seen::with_dead(vec![
        JobName::Fixed(FixedJob::SummarizeConversation),
        JobName::Fixed(FixedJob::EmbedTarget),
    ]));
    let settings = settings();
    let token = cancel();
    let payload = payload_of(row.conversation_id, SummaryTrigger::Threshold);
    for _ in 0..3 {
        let env = bind(
            &store,
            &script,
            &jobs,
            &settings,
            at("2026-10-04T12:10:00Z"),
            &token,
        );
        let error = handle_summarize(&env, &payload).await.expect_err("400");
        assert_eq!(error.to_string(), "LLM HTTP 400 (not retrying)");
    }
    assert!(
        recorded
            .logs
            .iter()
            .any(|line| line.contains("LLM HTTP 400 (not retrying)"))
    );
    assert_eq!(store.written.lock().await.len(), 0);
    let reap = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-04T13:51:00Z"),
        &token,
    );
    assert_eq!(handle_reap(&reap).await.expect("reap"), 2);
    assert_eq!(jobs.dead_count(FixedJob::SummarizeConversation).await, 0);
    assert_eq!(jobs.dead_count(FixedJob::EmbedTarget).await, 0);
    let outcome = handle_summarize(&reap, &payload).await.expect("retried");
    assert_eq!(outcome.summaries, 1);
    assert_rows(&store.written.lock().await, &recorded.tables.summaries);
}

fn floor_windows(recorded: &Scenario) -> Vec<ConversationWindow> {
    let floor = recorded
        .tables
        .summaries
        .iter()
        .find(|row| row.message_count == 6)
        .expect("floor");
    let few = recorded
        .tables
        .summaries
        .iter()
        .find(|row| row.message_count == 3)
        .expect("few");
    vec![
        window(
            floor.conversation_id,
            "cli:floor",
            "Session cli:floor",
            messages(floor.earliest_at, 6),
        ),
        window(
            few.conversation_id,
            "cli:few",
            "Session cli:few",
            messages(few.earliest_at, 3),
        ),
    ]
}

#[tokio::test]
async fn idle_and_stale_floors_follow_the_recording() {
    let fixture = fixture();
    let recorded = scenario(&fixture, recorded_name(5));
    let store = Arc::new(MemStore::new(floor_windows(recorded)));
    let script = Arc::new(Scripted::success());
    let jobs = Arc::new(Seen::default());
    let settings = settings();
    let token = cancel();
    let early = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-04T12:09:00Z"),
        &token,
    );
    assert_eq!(handle_enqueue_idle(&early).await.expect("early idle"), 0);
    assert_eq!(handle_enqueue_stale(&early).await.expect("early stale"), 0);
    assert_health(
        &store,
        &settings,
        health_steps(recorded).first().expect("early health"),
    )
    .await;
    let idle = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-04T12:25:00Z"),
        &token,
    );
    assert_eq!(handle_enqueue_idle(&idle).await.expect("idle"), 1);
    let job = jobs.only().await;
    assert_eq!(read_trigger(&job.payload), SummaryTrigger::SessionIdle);
    assert_eq!(job.priority, -20);
    assert_eq!(job.max_attempts, Some(3));
    let outcome = handle_summarize(&idle, &job.payload)
        .await
        .expect("idle summarize");
    assert_eq!(outcome.summaries, 1);
    let stale = bind(
        &store,
        &script,
        &jobs,
        &settings,
        at("2026-10-08T12:25:00Z"),
        &token,
    );
    assert_eq!(handle_enqueue_idle(&stale).await.expect("later idle"), 0);
    assert_eq!(handle_enqueue_stale(&stale).await.expect("stale"), 1);
    let job = jobs.only().await;
    assert_eq!(read_trigger(&job.payload), SummaryTrigger::SessionStale);
    assert_eq!(job.priority, 10);
    assert_eq!(job.max_attempts, Some(3));
    assert_eq!(job.key_mode, JobKeyMode::Arm);
    let outcome = handle_summarize(&stale, &job.payload)
        .await
        .expect("stale summarize");
    assert_eq!(outcome.summaries, 1);
    assert_calls(&script.calls.lock().await, &chat_calls(recorded));
    assert_rows(&store.written.lock().await, &recorded.tables.summaries);
    assert_health(
        &store,
        &settings,
        health_steps(recorded).get(1).expect("later health"),
    )
    .await;
    assert_eq!(summarized_counts(&recorded.logs), vec![1, 1]);
}

#[test]
fn summary_settings_reject_an_empty_batch() {
    let error = SummarySettings::from_parts(SummaryParts {
        model: "fake-chat".to_owned(),
        leaf_batch: 0,
        branch_batch: 8,
        root_batch: 5,
        min_leaves_for_branch: 5,
        min_branches_for_root: 3,
        idle_minutes: 15,
        stale_minutes: 5_760,
        stale_min_batch: 2,
        max_leaf_rounds: 10,
        pipeline_version: 5,
    })
    .expect_err("empty batch");
    assert!(error.to_string().contains("leaf batch"));
    assert_eq!(settings().idle_floor(), 5);
}

#[tokio::test]
async fn a_first_leaf_enqueues_filing() {
    let id = Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("id");
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
    assert_eq!(outcome.summaries, 1);
    assert!(!outcome.reenqueued);
    let [job] = <[Job; 1]>::try_from(jobs.filing().await).expect("one filing job");
    let key = format!("file-{id}");
    assert_eq!(job.name, JobName::Fixed(FixedJob::FileConversation));
    assert_eq!(job.key.as_deref(), Some(key.as_str()));
    assert_eq!(job.key_mode, JobKeyMode::Replace);
    assert_eq!(job.priority, 6);
    assert!(job.run_at.is_none());
    assert_eq!(workers::file_target(&job.payload).expect("payload"), id);
}

#[tokio::test]
async fn recent_conversations_rank_behind_the_sweeps_and_ahead_of_old_backlog() {
    let now = at("2026-10-30T18:00:00Z");
    let ago = |hours: i64| {
        now.checked_sub_signed(Duration::hours(hours))
            .expect("message time")
    };
    let [recent_idle, recent_stale, old_idle, old_stale] = [1, 2, 3, 4].map(conversation_id);
    let standard = settings();
    let quick_stale = stale_after_an_hour();
    let backlog = MemStore::new(vec![
        window(old_idle, "cli:old-idle", "Old idle", messages(ago(72), 10)),
        window(
            old_stale,
            "cli:old-stale",
            "Old stale",
            messages(ago(120), 3),
        ),
        window(
            recent_idle,
            "cli:recent-idle",
            "Recent idle",
            messages(ago(1), 10),
        ),
    ]);
    let tuned = MemStore::new(vec![window(
        recent_stale,
        "cli:recent-stale",
        "Recent stale",
        messages(ago(2), 3),
    )]);
    let script = Scripted::success();
    let jobs = Seen::default();
    let token = cancel();
    let env = bind(&backlog, &script, &jobs, &standard, now, &token);
    assert_eq!(handle_enqueue_idle(&env).await.expect("idle sweep"), 2);
    assert_eq!(handle_enqueue_stale(&env).await.expect("stale sweep"), 1);
    let env = bind(&tuned, &script, &jobs, &quick_stale, now, &token);
    assert_eq!(
        handle_enqueue_stale(&env).await.expect("tuned stale sweep"),
        1
    );
    let sweep = WorkerRole::Summarizer
        .crontab(false)
        .iter()
        .map(|entry| entry.job().priority)
        .max()
        .expect("summarizer sweeps");
    let ranks = [
        sweep,
        queued_priority(&jobs, recent_idle).await,
        queued_priority(&jobs, recent_stale).await,
        queued_priority(&jobs, old_idle).await,
        queued_priority(&jobs, old_stale).await,
    ];
    assert!(
        ranks.is_sorted_by(|earlier, later| earlier < later),
        "{ranks:?}"
    );
}

fn conversation_id(index: u8) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-0000000000{index:02}")).expect("id")
}

fn stale_after_an_hour() -> SummarySettings {
    SummarySettings::from_parts(SummaryParts {
        model: "fake-chat".to_owned(),
        leaf_batch: 10,
        branch_batch: 8,
        root_batch: 5,
        min_leaves_for_branch: 5,
        min_branches_for_root: 3,
        idle_minutes: 15,
        stale_minutes: 60,
        stale_min_batch: 2,
        max_leaf_rounds: 10,
        pipeline_version: 5,
    })
    .expect("stale settings")
}

async fn queued_priority(jobs: &Seen, id: Uuid) -> i16 {
    let key = format!("compact-{id}");
    jobs.jobs
        .lock()
        .await
        .iter()
        .find(|held| held.job.key.as_deref() == Some(key.as_str()))
        .map(|held| held.job.priority)
        .expect("a queued summary")
}

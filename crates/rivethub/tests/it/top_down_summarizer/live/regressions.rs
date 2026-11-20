use kernel::memory::summary_backend::SummaryArtifact;

use super::*;

pub(super) async fn wrong_width(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let script = Scripted::live();
    let permits = Semaphore::new(3);
    let session = "wrong-width";
    let id = capture(store, session, short("The first saffron receipt.")).await?;
    let request = settle(store, session).await?;
    run(store, &script, &request, 1, 131_072, &permits).await?;
    let root = store.live_input(id).await?.ok_or("tree")?.expected_root;
    assert!(root.is_some());
    capture(store, session, short("An amended receipt.")).await?;
    let next = settle(store, session).await?;
    for (attempt, width) in [(1, 1536), (3, 2)] {
        let bad = Scripted {
            embed_width: Some(width),
            ..Scripted::live()
        };
        let result = run(store, &bad, &next, attempt, 131_072, &permits).await;
        let Err(error) = result else {
            return Err("bad vector must hold publication".into());
        };
        assert!(matches!(error, WorkerError::Embed(_)));
        assert!(
            error
                .to_string()
                .contains(&format!("{width} dimensions; expected 1024"))
        );
        assert_eq!(
            store.live_input(id).await?.ok_or("tree")?.expected_root,
            root
        );
        assert_eq!(
            store
                .summary_state(id)
                .await?
                .ok_or("state")?
                .summarized_through,
            request.revision
        );
    }
    assert_eq!(
        store
            .summary_state(id)
            .await?
            .ok_or("state")?
            .failed_revision,
        next.revision
    );
    Ok(())
}

pub(super) async fn checkpoint_inactive_run(
    store: &(impl SummaryStore + MemoryStore),
) -> TestResult {
    let session = "checkpoint-ledger";
    let id = capture(store, session, short("An unsigned receipt.")).await?;
    let first = settle(store, session).await?;
    let revision = first.revision.ok_or("revision")?;
    let artifact = SummaryArtifact::encode(&"original")?;
    let run = store
        .freeze_live_run(id, revision, None, artifact.clone())
        .await?;
    let key = [5; 32];
    store
        .save_live_checkpoint(run, revision, key, artifact.clone())
        .await?;
    store
        .save_live_checkpoint(run, revision, key, SummaryArtifact::encode(&"duplicate")?)
        .await?;
    let saved = store
        .live_checkpoint(run, key)
        .await?
        .ok_or("saved artifact")?;
    assert_eq!(saved.decode::<String>()?, "original");
    capture(store, session, short("The signed receipt.")).await?;
    let next = settle(store, session).await?;
    store
        .freeze_live_run(
            id,
            next.revision.ok_or("new revision")?,
            None,
            artifact.clone(),
        )
        .await?;
    assert!(
        store
            .save_live_checkpoint(run, revision, [6; 32], artifact.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .save_live_checkpoint(Uuid::new_v4(), revision, key, artifact)
            .await
            .is_err()
    );
    assert!(store.live_checkpoint(run, [6; 32]).await?.is_none());
    Ok(())
}

pub(super) async fn capture_during_last_embedding(
    store: &(impl SummaryStore + MemoryStore),
) -> TestResult {
    capture_before_swap(store, "capture-before-swap", false).await?;
    capture_before_swap(store, "settle-before-swap", true).await
}

async fn capture_before_swap(
    store: &(impl SummaryStore + MemoryStore),
    session: &str,
    resettle: bool,
) -> TestResult {
    let id = capture(store, session, short("First receipt.")).await?;
    let first = settle(store, session).await?;
    let barrier = Arc::new(Barrier::new(2));
    let script = Scripted {
        embed_pause: Some(barrier.clone()),
        ..Scripted::live()
    };
    let permits = Semaphore::new(3);
    let capture_more = async {
        barrier.wait().await;
        capture(store, session, short("An amendment during publication.")).await?;
        assert_eq!(
            store
                .summary_state(id)
                .await?
                .ok_or("active state")?
                .settled_at,
            None
        );
        if resettle {
            let next = settle(store, session).await?;
            assert_ne!(next.revision, first.revision);
        }
        barrier.wait().await;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (stale, captured) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            run(store, &script, &first, 1, 131_072, &permits),
            capture_more
        )
    })
    .await?;
    captured?;
    stale?;
    assert!(
        store
            .live_input(id)
            .await?
            .ok_or("tree")?
            .expected_ids
            .is_empty()
    );
    assert_eq!(
        store
            .summary_state(id)
            .await?
            .ok_or("state")?
            .summarized_through,
        None
    );
    let next = settle(store, session).await?;
    let script = Scripted::live();
    run(store, &script, &next, 1, 131_072, &permits).await?;
    let calls = script.calls.lock().await;
    let call = calls.first().ok_or("re-settled prompt")?;
    assert!(call.user.contains("First receipt."));
    assert!(call.user.contains("An amendment during publication."));
    drop(calls);
    Ok(())
}

pub(super) async fn tokenizer_fallback(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    for (session, tokenizer) in [
        (
            "calibration-failure",
            (|_, _| None) as fn(usize, usize) -> Option<usize>,
        ),
        ("packing-failure", |call, chars| {
            (call == 0).then_some(chars)
        }),
        ("preflight-failure", |call, chars| {
            (call < 2).then_some(chars / 3)
        }),
        ("zero-exact-count", |call, chars| {
            Some(if call == 1 { 0 } else { chars / 3 })
        }),
    ] {
        let id = capture(store, session, short(&"cedar ".repeat(500))).await?;
        let request = settle(store, session).await?;
        let script = Scripted {
            tokenizer: Some(tokenizer),
            ..Scripted::live()
        };
        run(store, &script, &request, 3, 32_768, &Semaphore::new(3)).await?;
        assert!(
            store
                .live_input(id)
                .await?
                .ok_or("tree")?
                .expected_root
                .is_some()
        );
        let state = store.summary_state(id).await?.ok_or("state")?;
        assert_eq!(state.failed_revision, None);
        if session == "zero-exact-count" {
            assert_eq!(script.part_calls.load(SeqCst), 2);
        }
    }
    Ok(())
}

fn long_transcript() -> Vec<(String, Timestamp)> {
    let base = Timestamp::now() - jiff::SignedDuration::from_mins(1);
    (1..=8)
        .map(|n| {
            (
                format!("Harvest {n} {}", "sunflower ".repeat(1_250)),
                base + jiff::SignedDuration::from_secs(n),
            )
        })
        .collect()
}

pub(super) async fn oversized_composition(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let session = "single-too-large-for-carry";
    let id = capture(store, session, long_transcript()).await?;
    let request = settle(store, session).await?;
    let title = "## Récolte du jardin [m00001-m00001]";
    let head = "é".repeat(28_000);
    let tail = "é".repeat(29_000);
    let record = format!("{head}\n\n{title}\n{tail}");
    let script = Scripted {
        part_reply: Some(record),
        ..Scripted::live()
    };
    run(store, &script, &request, 1, 32_768, &Semaphore::new(3)).await?;
    let calls = script.calls.lock().await;
    let leaves: Vec<_> = calls
        .iter()
        .filter(|call| call.kind == "TOP_DOWN_CARRY" && call.user.contains('é'))
        .flat_map(|call| call.user.split("## Record of part "))
        .filter_map(|chunk| chunk.split_once("\n\n").map(|(_, body)| body))
        .collect();
    assert!(
        leaves
            .iter()
            .any(|leaf| leaf.trim_end() == format!("{title}\n{tail}"))
    );
    assert!(
        leaves
            .iter()
            .all(|leaf| !leaf.contains("## Récolte") || leaf.contains(title))
    );
    drop(calls);
    assert!(
        store
            .live_input(id)
            .await?
            .ok_or("tree")?
            .expected_root
            .is_some()
    );
    Ok(())
}

fn sqlite(dir: &tempfile::TempDir) -> TestResult<memory_sqlite::SqliteStore> {
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    Ok(memory_sqlite::SqliteStore::open(&path)?.with_summary_index_key([7; 32]))
}

#[tokio::test]
async fn top_down_checkpoint_rejects_inactive_run() -> TestResult {
    checkpoint_inactive_run(&sqlite(&tempfile::tempdir()?)?).await
}

#[tokio::test]
async fn top_down_tokenizer_falls_back_on_failure() -> TestResult {
    tokenizer_fallback(&sqlite(&tempfile::tempdir()?)?).await
}

#[tokio::test]
async fn top_down_live_oversized_composition() -> TestResult {
    oversized_composition(&sqlite(&tempfile::tempdir()?)?).await
}

#[tokio::test]
async fn top_down_reopen_part_prompts() -> TestResult {
    parts_and_late_input(&sqlite(&tempfile::tempdir()?)?).await
}

pub(super) async fn reject_wrong_width_repair(store: &dyn SummaryStore, id: Uuid) -> TestResult {
    use kernel::memory::summary_backend::{SummaryIndex, SummaryPassage};

    let expected = store.repair_input(id).await?.ok_or("marked index")?;
    let characters = i32::try_from(expected.content.chars().count())?;
    let at = Timestamp::now();
    let index = SummaryIndex {
        pooled: vec![0.25; 1024],
        passages: vec![SummaryPassage {
            content: expected.content.clone(),
            chars: 0..characters,
            earliest_at: at,
            latest_at: at,
            vector: vec![0.25; 12],
        }],
    };
    assert!(store.repair_index(expected, index).await.is_err());
    assert!(store.repair_input(id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn top_down_preflight_rejects_confirmed_context_overflow() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = sqlite(&dir)?;
    for (session, tokenizer) in [
        (
            "packing-over-context",
            (|call, chars| Some(if call < 1 { chars / 3 } else { 50_000 }))
                as fn(usize, usize) -> Option<usize>,
        ),
        ("preflight-over-context", |call, chars| {
            Some(if call < 2 { chars / 3 } else { 50_000 })
        }),
    ] {
        let id = capture(&store, session, short("Document the fig harvest.")).await?;
        let request = settle(&store, session).await?;
        let script = Scripted {
            tokenizer: Some(tokenizer),
            ..Scripted::live()
        };
        let result = run(&store, &script, &request, 1, 32_768, &Semaphore::new(3)).await;
        assert!(matches!(result, Err(WorkerError::SummarySettings { .. })));
        assert!(script.calls.lock().await.is_empty());
        assert!(
            store
                .live_input(id)
                .await?
                .ok_or("tree")?
                .expected_ids
                .is_empty()
        );
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct FrozenMeter {
    ratio: f64,
}

#[tokio::test]
async fn top_down_resume_preserves_frozen_calibration() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = sqlite(&dir)?;
    let session = "meter-resume";
    let id = capture(&store, session, short("One measured quince harvest.")).await?;
    let request = settle(&store, session).await?;
    let permits = Semaphore::new(3);
    let first = Scripted {
        tokenizer: Some(|_, chars| Some(chars)),
        ..Scripted::live()
    };
    first.fail_embeddings.store(true, SeqCst);
    assert!(matches!(
        run(&store, &first, &request, 1, 32_768, &permits).await,
        Err(WorkerError::Embed(_))
    ));
    let frozen = store.live_run(id).await?.ok_or("frozen workspace")?;
    let ratio = frozen.manifest.decode::<FrozenMeter>()?.ratio;
    assert!((ratio - 0.9).abs() < f64::EPSILON);
    let retry = Scripted {
        tokenizer: Some(|_, chars| Some(chars / 3)),
        ..Scripted::live()
    };
    retry.fail_embeddings.store(true, SeqCst);
    assert!(matches!(
        run(&store, &retry, &request, 2, 32_768, &permits).await,
        Err(WorkerError::Embed(_))
    ));
    let continued = store.live_run(id).await?.ok_or("continued workspace")?;
    assert_eq!(
        continued.manifest.decode::<FrozenMeter>()?.ratio.to_bits(),
        ratio.to_bits()
    );
    assert!(retry.tokenizations.lock().await.is_empty());
    assert!(retry.calls.lock().await.is_empty());
    retry.fail_embeddings.store(false, SeqCst);
    run(&store, &retry, &request, 3, 32_768, &permits).await?;
    Ok(())
}

#[tokio::test]
async fn top_down_composition_fails_closed_for_an_indivisible_heading() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = sqlite(&dir)?;
    let session = "oversized-indivisible-heading";
    let id = capture(&store, session, long_transcript()).await?;
    let request = settle(&store, session).await?;
    let script = Scripted {
        part_reply: Some(format!("## {}", "é".repeat(80_000))),
        ..Scripted::live()
    };
    let result = run(&store, &script, &request, 1, 32_768, &Semaphore::new(3)).await;
    let Err(error) = result else {
        return Err("the whole heading must fit".into());
    };
    assert!(
        error
            .to_string()
            .contains("one composition record exceeds its context budget")
    );
    assert!(
        script
            .calls
            .lock()
            .await
            .iter()
            .all(|call| call.kind == "TOP_DOWN_PART")
    );
    assert!(
        store
            .live_input(id)
            .await?
            .ok_or("tree")?
            .expected_ids
            .is_empty()
    );
    Ok(())
}

const PERMIT_CHILD_URL: &str = "RIVETHUB_TEST_SUMMARY_PERMIT_URL";
const PERMIT_READY: &str = "Holding the only summary model permit";

#[tokio::test]
#[ignore = "subprocess of the PostgreSQL publication test"]
async fn summary_permit_child() -> TestResult {
    let env = config::Environment::from_process();
    let url = env
        .text(PERMIT_CHILD_URL)
        .ok_or("child permit database URL")?;
    let store = memory_postgres::PostgresMemory::new(
        memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
            .connect(url)
            .await?,
    );
    let _permit = store.try_summary_permit(1).await?.ok_or("child permit")?;
    println!("{PERMIT_READY}");
    std::future::pending::<TestResult>().await
}

pub(super) async fn dead_process_releases_permit(pool: &sqlx::PgPool) -> TestResult {
    use sqlx::ConnectOptions as _;
    use std::process::Stdio;

    let url = pool.connect_options().to_url_lossy();
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--nocapture",
            "--exact",
            "top_down_summarizer::live::regressions::summary_permit_child",
        ])
        .env(PERMIT_CHILD_URL, url.as_str())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    wait_for_child_permit(child.stdout.take().ok_or("child stdout")?).await?;
    let store = memory_postgres::PostgresMemory::new(pool.clone());
    assert!(store.try_summary_permit(1).await?.is_none());
    child.kill().await?;
    assert!(!child.wait().await?.success());
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(30));
        loop {
            ticks.tick().await;
            if store.try_summary_permit(1).await?.is_some() {
                return Ok::<_, kernel::memory::SummaryStoreError>(());
            }
        }
    })
    .await??;
    Ok(())
}

async fn wait_for_child_permit(stdout: tokio::process::ChildStdout) -> TestResult {
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    let mut lines = BufReader::new(stdout).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = lines.next_line().await? {
            if line.contains(PERMIT_READY) {
                return Ok(());
            }
        }
        Err(std::io::Error::other(
            "child exited before acquiring a permit",
        ))
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn top_down_sqlite_swap_does_not_reuse_a_short_vector() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = sqlite(&dir)?;
    let session = "index-reuse-ledger";
    let id = capture(&store, session, short("One quince receipt.")).await?;
    let input = store.live_input(id).await?.ok_or("source")?;
    let root = Uuid::new_v4();
    let revision = store
        .summary_state(id)
        .await?
        .ok_or("captured revision")?
        .input_revision;
    let mut tree = single_record_tree(id, root, input, revision)?;
    store.replace_summary_tree(tree.clone()).await?;
    store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_summaries SET embedding = zeroblob(8), index_needs_repair = 0
             WHERE id = ?1",
                [root.to_string()],
            )
            .map_err(memory_sqlite::SqliteError::Pragma)?;
            Ok(())
        })
        .await?;
    let next = Uuid::new_v4();
    tree.expected_ids.insert(root);
    tree.records.first_mut().ok_or("replacement record")?.id = next;
    store.replace_summary_tree(tree).await?;
    assert!(store.repair_input(next).await?.is_some());
    let flags = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT embedding IS NULL, index_needs_repair FROM ros_summaries WHERE id = ?1",
                [next.to_string()],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(memory_sqlite::SqliteError::Pragma)
        })
        .await?;
    assert_eq!(flags, (true, true));
    Ok(())
}

fn single_record_tree(
    conversation: Uuid,
    root: Uuid,
    input: kernel::memory::summary_backend::SummaryInput,
    revision: i64,
) -> TestResult<kernel::memory::summary_reference::PreparedSummaryTree> {
    use kernel::memory::{
        SummaryKind,
        summary_reference::{PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry},
    };

    let message = input
        .messages
        .first()
        .and_then(|row| row.id.as_deref())
        .ok_or("message ID")?;
    Ok(PreparedSummaryTree {
        conversation,
        owner: input.owner,
        expected_ids: input.expected_ids,
        expected_revision: Some(revision),
        historical: false,
        created_at: Timestamp::now(),
        records: vec![PreparedSummaryRecord {
            id: root,
            parent: None,
            kind: SummaryKind::Session,
            ordinal: 0,
            content: "A quince receipt was recorded.".into(),
            input_key: [3; 32],
            index_key: Some([7; 32]),
            model: None,
            issued_part_ordinal: None,
            input_tokens: None,
            quality_flags: 0,
            section: None,
            sources: vec![SummaryManifestEntry {
                message: message.parse()?,
                ordinal: 1,
                render_key: Some([4; 32]),
            }],
        }],
        retained_parts: std::collections::BTreeSet::new(),
        part_order: Vec::new(),
        passage_windows: std::collections::BTreeMap::new(),
    })
}

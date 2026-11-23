use std::time::Duration;

use kernel::queue::JobQueue as _;
use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;

use crate::common::TestResult;
use crate::common::fresh_store;
use crate::common::seed;

#[tokio::test]
async fn pausing_twice_keeps_the_first_and_resuming_twice_is_harmless() -> TestResult {
    let fresh = fresh_store()?;
    testkit::maintenance::pausing_twice_keeps_the_first_and_resume_is_harmless(&fresh.store).await
}

#[tokio::test]
async fn a_role_outside_the_check_is_rejected() -> TestResult {
    let fresh = fresh_store()?;
    let inserted = fresh
        .store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO rh_worker_pauses (role, paused_at, paused_by)
                 VALUES ('summarizer', '2026-10-01T00:00:00.000Z', 'x')",
                [],
            )
            .map_err(seed)
        })
        .await;
    match inserted {
        Ok(rows) => Err(format!("summarizer was stored in {rows} row(s)").into()),
        Err(error) => {
            let text = sources(&error);
            if text.to_ascii_lowercase().contains("check") {
                Ok(())
            } else {
                Err(text.into())
            }
        }
    }
}

#[tokio::test]
async fn another_connection_sees_pause_and_resume() -> TestResult {
    let (directory, reader, writer) = two_stores()?;
    let seen =
        testkit::maintenance::another_connection_sees_pause_and_resume(&reader, &writer).await;
    drop(directory);
    seen
}

#[tokio::test]
async fn resume_from_another_connection_wakes_the_subscriber() -> TestResult {
    let (directory, listening, writer) = two_stores()?;
    let woken =
        testkit::maintenance::resume_from_another_connection_wakes(&listening, &writer).await;
    drop(directory);
    woken
}

fn two_stores() -> TestResult<(
    tempfile::TempDir,
    memory_sqlite::SqliteStore,
    memory_sqlite::SqliteStore,
)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let reader = memory_sqlite::SqliteStore::open(&path)?;
    let writer = memory_sqlite::SqliteStore::open(&path)?;
    Ok((directory, reader, writer))
}

#[tokio::test]
async fn cancelling_the_first_subscription_does_not_disable_later_ones() -> TestResult {
    use std::future::poll_fn;
    use std::task::Poll;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let listening = memory_sqlite::SqliteStore::open(&path)?;
    let writer = memory_sqlite::SqliteStore::open(&path)?;
    writer.pause(PausableRole::WikiBuilder, "external").await?;
    {
        let mut subscribe = listening.subscribe();
        poll_fn(|context| {
            assert!(subscribe.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    let mut notifications = listening.subscribe().await?;
    writer.resume(PausableRole::WikiBuilder).await?;
    assert_eq!(listening.paused().await?.len(), 0);
    tokio::time::timeout(Duration::from_secs(3), notifications.recv()).await??;
    Ok(())
}

#[tokio::test]
async fn a_local_pause_then_an_external_resume_wakes_the_subscriber() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let listening = memory_sqlite::SqliteStore::open(&path)?;
    let writer = memory_sqlite::SqliteStore::open(&path)?;
    let mut notifications = listening.subscribe().await?;
    listening
        .pause(kernel::queue::pause::PausableRole::WikiBuilder, "local")
        .await?;
    tokio::time::timeout(Duration::from_secs(2), notifications.recv()).await??;
    assert_eq!(listening.paused().await?.len(), 1);
    writer
        .resume(kernel::queue::pause::PausableRole::WikiBuilder)
        .await?;
    assert_eq!(listening.paused().await?.len(), 0);
    tokio::time::timeout(Duration::from_secs(3), notifications.recv()).await??;
    Ok(())
}

#[tokio::test]
async fn a_pause_and_resume_from_another_connection_wakes_the_subscriber() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let first = memory_sqlite::SqliteStore::open(&path)?;
    let second = memory_sqlite::SqliteStore::open(&path)?;
    wake_on(&first, &second, true).await?;
    wake_on(&first, &second, false).await?;
    Ok(())
}

#[tokio::test]
async fn an_unrelated_commit_does_not_wake_the_subscriber() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let listening = memory_sqlite::SqliteStore::open(&path)?;
    let writer = memory_sqlite::SqliteStore::open(&path)?;
    let mut notes = listening.subscribe().await?;
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    let _waiting = tokio::spawn(async move {
        let woke = notes.recv().await;
        match sender.send(woke) {
            Ok(()) => {}
            Err(missed) => drop(missed),
        }
    });
    writer
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO rh_tasks_archive (id, origin)
                 VALUES ('00000000-0000-4000-8000-000000000099', 'eval')",
                [],
            )
            .map_err(seed)?;
            Ok(())
        })
        .await?;
    match tokio::time::timeout(Duration::from_millis(1500), &mut receiver).await {
        Ok(woke) => Err(format!("unrelated commit woke the subscriber: {woke:?}").into()),
        Err(elapsed) => {
            let _: tokio::time::error::Elapsed = elapsed;
            writer.pause(PausableRole::WikiBuilder, "other").await?;
            tokio::time::timeout(Duration::from_secs(2), receiver).await???;
            Ok(())
        }
    }
}

async fn wake_on(
    listening: &memory_sqlite::SqliteStore,
    writer: &memory_sqlite::SqliteStore,
    pause: bool,
) -> TestResult {
    let mut notes = listening.subscribe().await?;
    let waiting = tokio::spawn(async move { notes.recv().await });
    tokio::task::yield_now().await;
    if pause {
        writer.pause(PausableRole::WikiBuilder, "other").await?;
    } else {
        writer.resume(PausableRole::WikiBuilder).await?;
    }
    tokio::time::timeout(Duration::from_secs(2), waiting).await???;
    Ok(())
}

fn sources(error: &memory_sqlite::SqliteError) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        text.push(' ');
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text
}

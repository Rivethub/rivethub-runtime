use kernel::memory::summary_opening::OpeningConsumer;
use workers::{ProcessRole, Runner, RunnerConfig};

use super::*;
use crate::common::{Queue, Store, deps, finish, wait_bool};

#[tokio::test]
async fn title_job_names_an_active_opening_and_preserves_a_supplied_title()
-> Result<(), Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    let manual = Uuid::new_v4();
    let request = "Trace the missing queue receipt";
    let store = Arc::new(MemStore::new());
    for (id, title, supplied) in [
        (id, "Session cli:receipt", false),
        (manual, "Session 123: hand picked…", true),
    ] {
        store.openings.lock().await.push(OpeningTitle {
            head: ConversationHead {
                id,
                session_key: "cli:receipt".to_owned(),
                agent: "rivet".to_owned(),
                channel: "cli".to_owned(),
                title: Some(title.to_owned()),
                trashed: false,
            },
            content: request.to_owned(),
            armed: true,
            supplied,
        });
    }

    let queue = Arc::new(Queue::new());
    for id in [id, manual] {
        queue.enqueue(OpeningConsumer::Title.job(id)?).await?;
    }
    let script = Arc::new(Scripted::default());
    let mut worker_deps = deps(Arc::new(Store::new()), script.clone());
    worker_deps.summaries = store.clone();
    let runner = Runner::new(
        queue,
        RunnerConfig::new(ProcessRole::Summarizer, 1)?,
        worker_deps,
    );
    let mut idle = runner.idle();
    let cancel = cancel();
    let run = tokio::spawn({
        let cancel = cancel.clone();
        async move { runner.run(cancel).await }
    });
    wait_bool(&mut idle, true).await;
    finish(cancel, run).await;

    let titled = store.opening_title(id).await?.expect("automatic opening");
    assert_eq!(
        titled.head.title.as_deref(),
        Some("Plan blue-green AcmeKit deploys")
    );
    assert!(!titled.armed);
    let named = store.opening_title(manual).await?.expect("manual opening");
    assert_eq!(
        named.head.title.as_deref(),
        Some("Session 123: hand picked…")
    );
    assert!(!named.armed);
    let calls = script.calls.lock().await;
    let [call] = calls.as_slice() else {
        return Err("expected one model call".into());
    };
    assert_eq!(call.kind, "TITLE_SYSTEM_PROMPT");
    assert_eq!(call.user, request);
    drop(calls);
    Ok(())
}

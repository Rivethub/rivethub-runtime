use std::path::Path;
use std::time::Duration;

use harness_acp::{AcpCommand, AcpHarnessAdapter, AcpLaunch, LaunchTable};
use kernel::harness::{HarnessAdapter, SessionOption, SessionSpec};
use kernel::harness::{HarnessModelsSource, HarnessOffering};
use wire::harness::HarnessId;
use wire::session_id::SessionId;

fn adapter(cwd: Option<&Path>, listing: bool) -> AcpHarnessAdapter {
    recording_adapter(cwd, listing, None)
}

fn recording_adapter(
    cwd: Option<&Path>,
    listing: bool,
    record: Option<&Path>,
) -> AcpHarnessAdapter {
    let mut args = vec![
        "--session-id".to_owned(),
        "ses_fake0123456789abcdefgh".to_owned(),
        "--on-load".to_owned(),
        "replay".to_owned(),
    ];
    if listing {
        args.push("--list-sessions".to_owned());
    }
    if let Some(record) = record {
        args.push("--record-new".to_owned());
        args.push(record.display().to_string());
        args.push("--record-delete".to_owned());
        args.push(record.with_extension("deleted").display().to_string());
    }
    AcpHarnessAdapter::with_launch_table(LaunchTable::from_entries([(
        HarnessId::Opencode,
        AcpLaunch::Native(AcpCommand {
            program: env!("CARGO_BIN_EXE_fake-acp-agent").into(),
            args,
            working_dir: cwd.map(Path::to_path_buf),
            env: Vec::new(),
        }),
    )]))
}

async fn discovered(
    adapter: &AcpHarnessAdapter,
) -> Result<Vec<HarnessOffering>, kernel::harness::HarnessAdapterError> {
    loop {
        let offerings = adapter.offerings().await?;
        if offerings.first().is_some_and(|offering| offering.chat) {
            return Ok(offerings);
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_uses_advertised_labels_current_model_and_session_listing() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let cwd = tempfile::tempdir().expect("cwd");
        for listing in [true, false] {
            let adapter = adapter(Some(cwd.path()), listing);
            let offerings = discovered(&adapter).await.expect("discovered");
            let offering = offerings.first().expect("offering");
            assert_eq!(offering.capabilities.observation.list_sessions, listing);
            assert_eq!(
                offering.capabilities.models_source,
                Some(HarnessModelsSource::Discovered)
            );
            let models = offering.capabilities.models.as_ref().expect("models");
            assert_eq!(models.len(), 3);
            let current = models
                .iter()
                .find(|model| model.is_default == Some(true))
                .expect("current model");
            assert_eq!(current.id, "grok-4.7");
            assert_eq!(current.label, "Grok 4.7 (recommended)");
            assert!(models.iter().all(|model| model.label != model.id));
        }
    })
    .await
    .expect("discovery timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_offerings_discover_once_in_the_discovery_dir() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().expect("root");
        let record = root.path().join("new-sessions");
        let discovery = root.path().join("state").join("discovery");
        let adapter = std::sync::Arc::new(
            recording_adapter(Some(root.path()), false, Some(&record))
                .with_discovery_dir(discovery.clone()),
        );
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let adapter = std::sync::Arc::clone(&adapter);
            calls.spawn(async move { adapter.offerings().await });
        }
        while let Some(offered) = calls.join_next().await {
            offered.expect("join").expect("offerings");
        }
        discovered(&adapter).await.expect("discovered");
        let recorded = std::fs::read_to_string(&record).expect("record");
        assert_eq!(
            recorded.lines().collect::<Vec<_>>(),
            vec![discovery.display().to_string()]
        );
        let deleted = std::fs::read_to_string(record.with_extension("deleted")).expect("deleted");
        assert_eq!(
            deleted.lines().collect::<Vec<_>>(),
            vec!["ses_fake0123456789abcdefgh"]
        );
    })
    .await
    .expect("discovery timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_that_never_answers_does_not_hold_up_offerings() {
    let cwd = tempfile::tempdir().expect("cwd");
    let adapter = AcpHarnessAdapter::with_launch_table(LaunchTable::from_entries([(
        HarnessId::Opencode,
        AcpLaunch::Native(AcpCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".to_owned(), "exec sleep 600".to_owned()],
            working_dir: Some(cwd.path().to_path_buf()),
            env: Vec::new(),
        }),
    )]));
    for _ in 0..3 {
        let offerings = tokio::time::timeout(Duration::from_secs(1), adapter.offerings())
            .await
            .expect("offerings answer while discovery hangs")
            .expect("offerings");
        assert!(!offerings.first().expect("offering").chat);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_selection_updates_the_advertised_default() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let cwd = tempfile::tempdir().expect("cwd");
        let adapter = adapter(None, true);
        let session = adapter
            .new_session(SessionSpec {
                session: SessionId::new(HarnessId::Opencode, "requested"),
                harness: HarnessId::Opencode,
                model: None,
                effort: None,
                working_dir: Some(cwd.path().to_path_buf()),
            })
            .await
            .expect("new session");
        adapter
            .set_option(&session, SessionOption::Model("grok-5".to_owned()))
            .await
            .expect("select");
        let offerings = adapter.offerings().await.expect("offerings");
        let models = offerings
            .first()
            .expect("offering")
            .capabilities
            .models
            .as_ref()
            .expect("models");
        let selected = models
            .iter()
            .find(|model| model.is_default == Some(true))
            .expect("selected model");
        assert_eq!(selected.id, "grok-5");
        assert_eq!(selected.label, "Grok 5");
        assert_eq!(
            models
                .iter()
                .filter(|model| model.is_default == Some(true))
                .count(),
            1
        );
        adapter.release(&session).await.expect("release");
    })
    .await
    .expect("session timeout");
}

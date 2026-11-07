use crate::common;

use common::TestResult;
use common::batch;
use common::fresh_store;
use common::instant;
use common::message;
use kernel::memory::MemoryStore;
use kernel::owner::Owner;
use wire::message::Role;

#[tokio::test]
async fn the_latest_capture_of_each_harness_is_read_by_its_session_key_prefix() -> TestResult {
    let fresh = fresh_store()?;
    let owner = Owner::new("alice".to_owned())?;
    let morning = instant("2026-11-05T08:00:00.000Z")?;
    let evening = instant("2026-11-05T20:00:00.000Z")?;
    for (key, agent, channel, at, event) in [
        ("claude-code:one", "rivet", "claude-code", morning, "evt-1"),
        ("claude-code:two", "rivet", "claude-code", evening, "evt-2"),
        ("codex:three", "rivet-gpt", "codex", morning, "evt-3"),
        (
            "kimi-code:four",
            "rivet-kimi",
            "kimi-code",
            evening,
            "evt-4",
        ),
    ] {
        let mut request = batch(key, agent, channel);
        request.messages.push(message(event, Role::User, "hello"));
        fresh.store.capture(request, &owner, None, at).await?;
    }
    let mut activity = fresh.store.capture_activity().await?;
    activity.sort_by(|a, b| a.key_prefix.cmp(&b.key_prefix));
    let seen: Vec<_> = activity
        .iter()
        .map(|entry| (entry.key_prefix.as_str(), entry.last_at))
        .collect();
    assert_eq!(
        seen,
        [
            ("claude-code", evening),
            ("codex", morning),
            ("kimi-code", evening)
        ]
    );
    Ok(())
}

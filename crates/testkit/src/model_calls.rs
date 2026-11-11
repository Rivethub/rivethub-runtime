use kernel::model_calls::{ModelCall, ModelCallStore, TurnMetadata};
use wire::model_call::{CallOutcome, CallUsage};
use wire::session_id::SessionId;

use crate::topics::{Outcome, at, same};

pub const CLAUDE_SESSION: &str = "claude-code:00000000-0000-4000-8000-0000000000c1";
pub const CODEX_SESSION: &str = "codex:00000000-0000-7000-8000-0000000000c2";

pub fn call(
    id: &str,
    minute: u32,
    session: Option<&str>,
) -> Result<ModelCall, wire::session_id::SessionIdError> {
    Ok(ModelCall {
        call_id: id.to_owned(),
        started_at: at(minute),
        finished_at: None,
        provider: "anthropic".to_owned(),
        model_requested: None,
        model_served: None,
        account: None,
        status_code: None,
        outcome: CallOutcome::Completed,
        retry_after_secs: None,
        usage: None,
        session: session.map(str::parse::<SessionId>).transpose()?,
        turn: None,
    })
}

pub async fn a_repeated_call_is_a_duplicate_and_changes_nothing(
    store: &dyn ModelCallStore,
) -> Outcome {
    let owner = crate::owner()?;
    let first = call(
        "00000000-0000-4000-8000-0000000000e1",
        0,
        Some(CLAUDE_SESSION),
    )?;
    let recorded = store.record(&owner, std::slice::from_ref(&first)).await?;
    same(
        "first record",
        (recorded.inserted, recorded.duplicate),
        (1, 0),
    )?;

    let mut changed = first.clone();
    changed.provider = "openai".to_owned();
    changed.outcome = CallOutcome::ServerError;
    changed.usage = Some(CallUsage {
        input: 9,
        ..CallUsage::default()
    });
    let repeat = store.record(&owner, &[changed.clone(), changed]).await?;
    same("repeat record", (repeat.inserted, repeat.duplicate), (0, 2))?;

    let session: SessionId = CLAUDE_SESSION.parse()?;
    same(
        "stored call",
        store.session_calls(&session).await?,
        vec![first],
    )?;
    Ok(())
}

pub async fn a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate(
    store: &dyn ModelCallStore,
) -> Outcome {
    let one = call(
        "00000000-0000-4000-8000-0000000000e1",
        0,
        Some(CLAUDE_SESSION),
    )?;
    let recorded = store.record(&crate::owner()?, &[one.clone(), one]).await?;
    same("record", (recorded.inserted, recorded.duplicate), (1, 1))?;
    Ok(())
}

pub async fn a_session_reads_its_own_calls_in_start_order_with_every_field(
    store: &dyn ModelCallStore,
) -> Outcome {
    let mut late = call(
        "00000000-0000-4000-8000-0000000000e3",
        20,
        Some(CODEX_SESSION),
    )?;
    late.finished_at = Some(at(22));
    late.model_requested = Some("alias".to_owned());
    late.model_served = Some("model-a".to_owned());
    late.account = Some("work".to_owned());
    late.status_code = Some(429);
    late.outcome = CallOutcome::RateLimited;
    late.retry_after_secs = Some(30);
    late.usage = Some(CallUsage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        reasoning: 5,
    });
    late.turn = Some(TurnMetadata {
        turn_id: Some("00000000-0000-7000-8000-0000000000d1".to_owned()),
        request_kind: Some("turn".to_owned()),
    });
    let early = call(
        "00000000-0000-4000-8000-0000000000e2",
        10,
        Some(CODEX_SESSION),
    )?;
    let other = call(
        "00000000-0000-4000-8000-0000000000e4",
        5,
        Some(CLAUDE_SESSION),
    )?;
    let unmatched = call("00000000-0000-4000-8000-0000000000e5", 1, None)?;
    store
        .record(
            &crate::owner()?,
            &[late.clone(), early.clone(), other, unmatched],
        )
        .await?;
    let session: SessionId = CODEX_SESSION.parse()?;
    same(
        "codex session",
        store.session_calls(&session).await?,
        vec![early, late],
    )?;
    Ok(())
}

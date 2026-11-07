#![cfg(test)]

use config::Environment;
use workers::{EmbedSettings, MemoryJobSettings, WorkerError};

fn with(name: &str, value: &str) -> Result<MemoryJobSettings, WorkerError> {
    MemoryJobSettings::from_environment(&Environment::from_vars([(name, value)]))
}

#[test]
fn unconfigured_sweeps_use_the_datahub_defaults() -> Result<(), WorkerError> {
    let expected = MemoryJobSettings {
        sweep_limit: 200,
        chunk_backfill_limit: 200,
        sweep_max_attempts: 5,
        wiki_sweep_limit: 20,
        flag_after: 5,
        backfill_max: 6,
        model_slots: 8,
        trash_days: 30,
        claim_ceiling: None,
        backfill_enabled: false,
    };
    assert_eq!(
        MemoryJobSettings::from_environment(&Environment::default())?,
        expected
    );
    assert_eq!(
        MemoryJobSettings::standard(),
        MemoryJobSettings {
            backfill_enabled: true,
            ..expected
        }
    );
    assert!(EmbedSettings::standard().chunks_enabled);
    assert_eq!(EmbedSettings::standard().chars_per_chunk, 6000);
    Ok(())
}

#[test]
fn environment_overrides_are_typed_and_the_wiki_cap_is_clamped() -> Result<(), WorkerError> {
    assert_eq!(with("RIVETHUB_EMBED_SWEEP_LIMIT", "50")?.sweep_limit, 50);
    assert_eq!(
        with("RIVETHUB_CHUNK_BACKFILL_LIMIT", "75")?.chunk_backfill_limit,
        75
    );
    assert_eq!(
        with("RIVETHUB_EMBED_SWEEP_MAX_ATTEMPTS", "9")?.sweep_max_attempts,
        9
    );
    assert_eq!(with("RIVETHUB_FLAG_AFTER_FAILURES", "3")?.flag_after, 3);
    assert_eq!(with("RIVETHUB_TRASH_DAYS", "7")?.trash_days, 7);
    assert_eq!(
        with("RIVETHUB_CLAIM_MAX_PRIORITY", "49")?.claim_ceiling,
        Some(49)
    );
    assert!(with("RIVETHUB_WIKI_BACKFILL", "1")?.backfill_enabled);
    assert_eq!(
        with("RIVETHUB_WIKI_SWEEP_LIMIT", "500")?.wiki_sweep_limit,
        200
    );
    assert_eq!(with("RIVETHUB_WIKI_SWEEP_LIMIT", "-4")?.wiki_sweep_limit, 0);
    assert_eq!(with("RIVETHUB_WIKI_SWEEP_LIMIT", "0")?.wiki_sweep_limit, 0);
    assert_eq!(
        with("RIVETHUB_WIKI_SWEEP_LIMIT", "35")?.wiki_sweep_limit,
        35
    );
    for (name, value) in [
        ("RIVETHUB_WIKI_SWEEP_LIMIT", "many"),
        ("RIVETHUB_CHUNK_BACKFILL_LIMIT", "0"),
        ("RIVETHUB_EMBED_SWEEP_LIMIT", "-1"),
        ("RIVETHUB_EMBED_SWEEP_MAX_ATTEMPTS", "3000000000"),
    ] {
        assert!(
            matches!(with(name, value), Err(WorkerError::JobSetting { .. })),
            "{name}={value}"
        );
    }
    Ok(())
}

#[test]
fn worker_concurrency_is_unset_by_default_and_must_be_a_positive_count() -> Result<(), WorkerError>
{
    let concurrency = |value: &str| {
        workers::worker_concurrency(&Environment::from_vars([(
            "RIVETHUB_WORKER_CONCURRENCY",
            value,
        )]))
    };
    assert_eq!(workers::worker_concurrency(&Environment::default())?, None);
    assert_eq!(concurrency("2")?, Some(2));
    for refused in ["0", "-1", "two", "4294967296"] {
        assert!(
            matches!(concurrency(refused), Err(WorkerError::JobSetting { ref name }) if name == "RIVETHUB_WORKER_CONCURRENCY"),
            "{refused}"
        );
    }
    Ok(())
}

#[test]
fn summary_caps_and_embed_timeout_follow_the_environment() -> Result<(), WorkerError> {
    let standard = workers::SummarySettings::standard("m")?;
    let unset = workers::tuned_summary(standard.clone(), &Environment::default())?;
    assert_eq!(
        (
            unset.leaf_max_tokens,
            unset.branch_max_tokens,
            unset.root_max_tokens
        ),
        (16_384, 24_576, 32_768)
    );
    let tuned = workers::tuned_summary(
        standard.clone(),
        &Environment::from_vars([
            ("RIVETHUB_SUMMARY_LEAF_TOKENS", "4096"),
            ("RIVETHUB_SUMMARY_BRANCH_TOKENS", "6144"),
            ("RIVETHUB_SUMMARY_ROOT_TOKENS", "8192"),
        ]),
    )?;
    assert_eq!(
        (
            tuned.leaf_max_tokens,
            tuned.branch_max_tokens,
            tuned.root_max_tokens
        ),
        (4096, 6144, 8192)
    );
    assert!(matches!(
        workers::tuned_summary(standard, &Environment::from_vars([("RIVETHUB_SUMMARY_ROOT_TOKENS", "0")])),
        Err(WorkerError::JobSetting { ref name }) if name == "RIVETHUB_SUMMARY_ROOT_TOKENS"
    ));
    let embed = workers::tuned_embed(
        EmbedSettings::standard(),
        &Environment::from_vars([("RIVETHUB_EMBED_TIMEOUT_MS", "60000")]),
    )?;
    assert_eq!(embed.timeout, std::time::Duration::from_secs(60));
    assert_eq!(
        workers::tuned_embed(EmbedSettings::standard(), &Environment::default())?.timeout,
        EmbedSettings::standard().timeout
    );
    Ok(())
}

#[test]
fn wiki_extraction_leaves_the_model_the_same_room_as_the_den() {
    assert_eq!(workers::wiki_builder::WIKI_EXTRACT_MAX_TOKENS, 32_000);
}

#[test]
fn the_backfill_grows_only_into_free_model_slots() {
    use kernel::model::ModelLoad;
    use workers::wiki_builder::backfill_budget;
    let load = |running, waiting| Some(ModelLoad { running, waiting });
    assert_eq!(backfill_budget(load(2, 0), 6, 8).idle, 6);
    assert_eq!(backfill_budget(load(5, 0), 6, 8).idle, 3);
    assert_eq!(backfill_budget(load(8, 0), 6, 8).idle, 1);
    assert_eq!(backfill_budget(load(2, 1), 6, 8).idle, 1);
    assert_eq!(backfill_budget(None, 6, 8).idle, 1);
    assert_eq!(backfill_budget(load(0, 0), 6, 8).busy, 1);
}

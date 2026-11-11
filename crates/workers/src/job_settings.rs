use config::Environment;

use crate::WorkerError;

const WIKI_SWEEP_CEILING: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryJobSettings {
    pub sweep_limit: u32,
    pub chunk_backfill_limit: u32,
    pub sweep_max_attempts: u32,
    pub wiki_sweep_limit: u32,
    pub flag_after: u32,
    pub backfill_max: u32,
    pub model_slots: u32,
    pub trash_days: u32,
    pub claim_ceiling: Option<i16>,
    pub backfill_enabled: bool,
}

impl MemoryJobSettings {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            sweep_limit: 200,
            chunk_backfill_limit: 200,
            sweep_max_attempts: 5,
            wiki_sweep_limit: 20,
            flag_after: kernel::flagged::FLAG_AFTER_FAILURES,
            backfill_max: 6,
            model_slots: 8,
            trash_days: kernel::memory::TRASH_RETENTION_DAYS,
            claim_ceiling: None,
            backfill_enabled: true,
        }
    }

    pub fn from_environment(env: &Environment) -> Result<Self, WorkerError> {
        let mut settings = Self::standard();
        for (name, slot) in [
            ("RIVETHUB_EMBED_SWEEP_LIMIT", &mut settings.sweep_limit),
            (
                "RIVETHUB_CHUNK_BACKFILL_LIMIT",
                &mut settings.chunk_backfill_limit,
            ),
            (
                "RIVETHUB_EMBED_SWEEP_MAX_ATTEMPTS",
                &mut settings.sweep_max_attempts,
            ),
            ("RIVETHUB_FLAG_AFTER_FAILURES", &mut settings.flag_after),
            ("RIVETHUB_WIKI_BACKFILL_MAX", &mut settings.backfill_max),
            ("RIVETHUB_MODEL_SLOTS", &mut settings.model_slots),
            ("RIVETHUB_TRASH_DAYS", &mut settings.trash_days),
        ] {
            if let Some(parsed) = number(env, name)? {
                if parsed == 0 || parsed > i64::from(i32::MAX) {
                    return Err(WorkerError::JobSetting {
                        name: name.to_owned(),
                    });
                }
                *slot = u32::try_from(parsed).map_err(|_| WorkerError::JobSetting {
                    name: name.to_owned(),
                })?;
            }
        }
        settings.backfill_enabled = env.text("RIVETHUB_WIKI_BACKFILL") == Some("1");
        let name = "RIVETHUB_CLAIM_MAX_PRIORITY";
        if let Some(parsed) = number(env, name)? {
            settings.claim_ceiling =
                Some(i16::try_from(parsed).map_err(|_| WorkerError::JobSetting {
                    name: name.to_owned(),
                })?);
        }
        let name = "RIVETHUB_WIKI_SWEEP_LIMIT";
        if let Some(parsed) = number(env, name)? {
            settings.wiki_sweep_limit =
                u32::try_from(parsed.clamp(0, i64::from(WIKI_SWEEP_CEILING))).map_err(|_| {
                    WorkerError::JobSetting {
                        name: name.to_owned(),
                    }
                })?;
        }
        Ok(settings)
    }
}

pub fn tuned_summary(
    settings: crate::SummarySettings,
    env: &Environment,
) -> Result<crate::SummarySettings, WorkerError> {
    let leaf = positive(env, "RIVETHUB_SUMMARY_LEAF_TOKENS")?.unwrap_or(settings.leaf_max_tokens);
    let branch =
        positive(env, "RIVETHUB_SUMMARY_BRANCH_TOKENS")?.unwrap_or(settings.branch_max_tokens);
    let root = positive(env, "RIVETHUB_SUMMARY_ROOT_TOKENS")?.unwrap_or(settings.root_max_tokens);
    settings.with_max_tokens(leaf, branch, root)
}

pub fn tuned_embed(
    settings: crate::EmbedSettings,
    env: &Environment,
) -> Result<crate::EmbedSettings, WorkerError> {
    Ok(match positive(env, "RIVETHUB_EMBED_TIMEOUT_MS")? {
        Some(ms) => settings.with_timeout(std::time::Duration::from_millis(u64::from(ms))),
        None => settings,
    })
}

fn positive(env: &Environment, name: &str) -> Result<Option<u32>, WorkerError> {
    let invalid = || WorkerError::JobSetting {
        name: name.to_owned(),
    };
    match number(env, name)? {
        None => Ok(None),
        Some(parsed) if parsed > 0 => u32::try_from(parsed).map(Some).map_err(|_| invalid()),
        Some(_) => Err(invalid()),
    }
}

pub fn worker_concurrency(env: &Environment) -> Result<Option<u32>, WorkerError> {
    positive(env, "RIVETHUB_WORKER_CONCURRENCY")
}

fn number(env: &Environment, name: &str) -> Result<Option<i64>, WorkerError> {
    let invalid = || WorkerError::JobSetting {
        name: name.to_owned(),
    };
    env.get(name)
        .map(|value| {
            value
                .to_str()
                .ok_or_else(invalid)?
                .parse::<i64>()
                .map_err(|_| invalid())
        })
        .transpose()
}

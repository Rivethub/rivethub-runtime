use kernel::wiki::entity_match::MATCH_THRESHOLD;
use kernel::wiki::entity_match::MatchAct;
use kernel::wiki::entity_match::TYPE_ASK_LIMIT;
use kernel::wiki::entity_match::gate;
use kernel::wiki::entity_match::same_type_name;
use kernel::wiki::entity_match::type_options;
use kernel::wiki::entity_pass::MatchDecision;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::TypeBrief;
use kernel::wiki::entity_pass::compact_key;
use uuid::Uuid;

use super::super::EntityBuilderEnv;
use super::super::decide::DecideRequest;
use super::super::decide::EntityDecider;
use super::super::decide::RawDecision;
use super::bind::ask;
use crate::WorkerError;

pub(super) enum TypeOutcome {
    Use {
        name: String,
        decision: MatchDecision,
    },
    Unsure {
        decision: MatchDecision,
    },
    Unreachable {
        decision: MatchDecision,
        error: WorkerError,
    },
}

pub(in crate::entity_builder) async fn settle_prepared_type(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    decider: &dyn EntityDecider,
    prepared: &mut PreparedEntity,
    catalogue: &[TypeBrief],
) -> Result<bool, WorkerError> {
    match settle_type(decider, &prepared.type_name, catalogue).await {
        TypeOutcome::Unreachable { decision, error } => {
            env.store
                .record_decision(summary, &decision, env.now)
                .await?;
            Err(error)
        }
        TypeOutcome::Unsure { decision } => {
            let confidence = decision.confidence;
            env.store
                .record_decision(summary, &decision, env.now)
                .await?;
            env.store
                .hold_candidate(summary, prepared, confidence, "type-unsure", env.now)
                .await?;
            Ok(false)
        }
        TypeOutcome::Use { name, decision } => {
            env.store
                .record_decision(summary, &decision, env.now)
                .await?;
            if let Some(alias) = alias_to_keep(&prepared.type_name, &name) {
                env.store.remember_type_alias(&name, &alias).await?;
            }
            prepared.type_name = name;
            Ok(true)
        }
    }
}

pub(super) async fn settle_type(
    decider: &dyn EntityDecider,
    wanted: &str,
    existing: &[TypeBrief],
) -> TypeOutcome {
    if let Some(name) = mechanical(wanted, existing) {
        let owned = name.to_owned();
        return TypeOutcome::Use {
            name: owned.clone(),
            decision: type_decision(
                wanted,
                std::slice::from_ref(&owned),
                "same",
                "",
                1.0,
                "exact",
            ),
        };
    }
    if existing.is_empty() {
        return TypeOutcome::Use {
            name: wanted.to_owned(),
            decision: type_decision(wanted, &[], "none", "", 0.0, "none"),
        };
    }
    let ranked = ranked_briefs(wanted, existing);
    let lines = ranked
        .iter()
        .map(|brief| type_line(brief))
        .collect::<Vec<_>>();
    let request = DecideRequest {
        observation: format!(
            "proposed type: {wanted}\nThis is a type name, not an entity. Choose the option that is the same kind, or \"new\" when it is a different kind."
        ),
        options: lines.clone(),
    };
    let decision = match ask(decider, &request).await {
        Ok(decision) => decision,
        Err(error) => {
            return TypeOutcome::Unreachable {
                decision: type_decision(
                    wanted,
                    &lines,
                    "unreachable",
                    "",
                    0.0,
                    decider.backend_name(),
                ),
                error: type_undecided(&error),
            };
        }
    };
    judged_type(wanted, &ranked, &lines, &decision, decider.backend_name())
}

fn mechanical<'a>(wanted: &str, existing: &'a [TypeBrief]) -> Option<&'a str> {
    let names = existing
        .iter()
        .map(|brief| brief.name.clone())
        .collect::<Vec<_>>();
    if let Some(name) = same_type_name(wanted, &names) {
        return existing
            .iter()
            .find(|brief| brief.name == name)
            .map(|brief| brief.name.as_str());
    }
    let folded = compact_key(wanted);
    if folded.is_empty() {
        return None;
    }
    unique_name(existing, |brief| compact_key(&brief.slug) == folded).or_else(|| {
        unique_name(existing, |brief| {
            brief
                .aliases
                .iter()
                .any(|alias| compact_key(alias) == folded)
        })
    })
}

fn unique_name(existing: &[TypeBrief], pred: impl Fn(&TypeBrief) -> bool) -> Option<&str> {
    let hits = existing
        .iter()
        .filter(|brief| pred(brief))
        .collect::<Vec<_>>();
    match hits.as_slice() {
        [only] => Some(only.name.as_str()),
        _ => None,
    }
}

fn ranked_briefs<'a>(wanted: &str, existing: &'a [TypeBrief]) -> Vec<&'a TypeBrief> {
    if existing.len() <= TYPE_ASK_LIMIT {
        return existing.iter().collect();
    }
    let names = existing
        .iter()
        .map(|brief| brief.name.clone())
        .collect::<Vec<_>>();
    type_options(wanted, &names)
        .into_iter()
        .filter_map(|name| existing.iter().find(|brief| brief.name == name))
        .collect()
}

fn type_line(brief: &TypeBrief) -> String {
    let mut notes = Vec::new();
    if !brief.aliases.is_empty() {
        notes.push(format!("aliases: {}", brief.aliases.join(", ")));
    }
    if !brief.examples.is_empty() {
        notes.push(format!("examples: {}", brief.examples.join(", ")));
    }
    if notes.is_empty() {
        brief.name.clone()
    } else {
        format!("{} ({})", brief.name, notes.join("; "))
    }
}

fn judged_type(
    wanted: &str,
    ranked: &[&TypeBrief],
    lines: &[String],
    decision: &RawDecision,
    model: &str,
) -> TypeOutcome {
    let act = gate(
        &decision.choice,
        decision.confidence,
        MATCH_THRESHOLD,
        lines.len(),
    );
    match act {
        MatchAct::Existing(index) => match ranked.get(index) {
            Some(brief) => TypeOutcome::Use {
                name: brief.name.clone(),
                decision: type_decision(
                    wanted,
                    lines,
                    "same",
                    &decision.choice,
                    decision.confidence,
                    model,
                ),
            },
            None => unsure(wanted, lines, decision, model),
        },
        MatchAct::New => TypeOutcome::Use {
            name: wanted.to_owned(),
            decision: type_decision(
                wanted,
                lines,
                "different",
                &decision.choice,
                decision.confidence,
                model,
            ),
        },
        MatchAct::Hold => unsure(wanted, lines, decision, model),
    }
}

fn unsure(wanted: &str, lines: &[String], decision: &RawDecision, model: &str) -> TypeOutcome {
    TypeOutcome::Unsure {
        decision: type_decision(
            wanted,
            lines,
            "hold",
            &decision.choice,
            decision.confidence,
            model,
        ),
    }
}

fn type_undecided(error: &WorkerError) -> WorkerError {
    WorkerError::Llm {
        message: format!("type match undecided: {error}"),
    }
}

pub(super) fn alias_to_keep(proposed: &str, chosen: &str) -> Option<String> {
    let proposed = proposed.trim();
    if proposed.is_empty() || proposed.eq_ignore_ascii_case(chosen.trim()) {
        None
    } else {
        Some(proposed.to_owned())
    }
}

fn type_decision(
    wanted: &str,
    options: &[String],
    verdict: &str,
    choice: &str,
    confidence: f32,
    model: &str,
) -> MatchDecision {
    MatchDecision {
        kind: "type".to_owned(),
        proposed_name: wanted.to_owned(),
        proposed_type: "type".to_owned(),
        candidates: options.to_vec(),
        verdict: verdict.to_owned(),
        choice: choice.to_owned(),
        confidence,
        model: model.to_owned(),
        entity_id: None,
    }
}

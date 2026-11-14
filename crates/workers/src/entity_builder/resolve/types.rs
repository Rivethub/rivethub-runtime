use kernel::wiki::entity_match::MATCH_THRESHOLD;
use kernel::wiki::entity_match::MatchAct;
use kernel::wiki::entity_match::gate;
use kernel::wiki::entity_match::same_type_name;
use kernel::wiki::entity_match::type_options;
use kernel::wiki::entity_pass::MatchDecision;
use kernel::wiki::entity_pass::PreparedEntity;
use uuid::Uuid;

use super::super::EntityBuilderEnv;
use super::super::decide::DecideRequest;
use super::super::decide::EntityDecider;
use super::bind::ask;
use super::bind::undecided;
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
    catalogue: &[String],
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
            prepared.type_name = name;
            Ok(true)
        }
    }
}

pub(super) async fn settle_type(
    decider: &dyn EntityDecider,
    wanted: &str,
    existing: &[String],
) -> TypeOutcome {
    if let Some(name) = same_type_name(wanted, existing) {
        let owned = name.to_owned();
        let decision = type_decision(
            wanted,
            std::slice::from_ref(&owned),
            "same",
            "",
            1.0,
            "exact",
        );
        return TypeOutcome::Use {
            name: owned,
            decision,
        };
    }
    if existing.is_empty() {
        return TypeOutcome::Use {
            name: wanted.to_owned(),
            decision: type_decision(wanted, &[], "none", "", 0.0, "none"),
        };
    }
    let options = type_options(wanted, existing)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let request = DecideRequest {
        observation: format!(
            "proposed type: {wanted}\nThis is a type name, not an entity. Choose the option that is the same kind, or \"new\" when it is a different kind."
        ),
        options: options.clone(),
    };
    let decision = match ask(decider, &request).await {
        Ok(decision) => decision,
        Err(error) => {
            return TypeOutcome::Unreachable {
                decision: type_decision(
                    wanted,
                    &options,
                    "unreachable",
                    "",
                    0.0,
                    decider.backend_name(),
                ),
                error: undecided(&error),
            };
        }
    };
    judged_type(wanted, &options, &decision, decider.backend_name())
}

fn judged_type(
    wanted: &str,
    options: &[String],
    decision: &super::super::decide::RawDecision,
    model: &str,
) -> TypeOutcome {
    let act = gate(
        &decision.choice,
        decision.confidence,
        MATCH_THRESHOLD,
        options.len(),
    );
    match act {
        MatchAct::Existing(index) => match options.get(index) {
            Some(name) => TypeOutcome::Use {
                name: name.clone(),
                decision: type_decision(
                    wanted,
                    options,
                    "same",
                    &decision.choice,
                    decision.confidence,
                    model,
                ),
            },
            None => unsure(wanted, options, decision, model),
        },
        MatchAct::New => TypeOutcome::Use {
            name: wanted.to_owned(),
            decision: type_decision(
                wanted,
                options,
                "different",
                &decision.choice,
                decision.confidence,
                model,
            ),
        },
        MatchAct::Hold => unsure(wanted, options, decision, model),
    }
}

fn unsure(
    wanted: &str,
    options: &[String],
    decision: &super::super::decide::RawDecision,
    model: &str,
) -> TypeOutcome {
    TypeOutcome::Unsure {
        decision: type_decision(
            wanted,
            options,
            "hold",
            &decision.choice,
            decision.confidence,
            model,
        ),
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

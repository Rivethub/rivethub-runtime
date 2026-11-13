use kernel::wiki::entity_match::MatchAct;
use kernel::wiki::entity_match::chosen_id;
use kernel::wiki::entity_match::exact_id;
use kernel::wiki::entity_match::gate;
use kernel::wiki::entity_match::name_identifies;
use kernel::wiki::entity_match::names_same_entity;
use kernel::wiki::entity_match::rank_candidates;
use kernel::wiki::entity_match::types_compatible;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::PreparedEntity;
use uuid::Uuid;

use super::super::EntityBuilderEnv;
use super::super::decide::DecideRequest;
use super::super::decide::EntityDecider;
use super::super::decide::RawDecision;
use super::super::rewrite;
use crate::WorkerError;

use super::alias::RefGate;
use super::alias::accept_existing;
use super::alias::apply_merges;
use super::alias::inject_suggested;
use super::alias::new_entity_aliases;
use super::alias::ref_gate;
use super::alias::subject_for;
use super::vectors::embed_observation;

#[derive(Debug)]
pub(in crate::entity_builder) struct Kept {
    pub(in crate::entity_builder) prepared: PreparedEntity,
    pub(in crate::entity_builder) embedding: Option<Vec<f32>>,
}

pub(super) enum MatchStep {
    Ready(PreparedEntity),
    Held { confidence: f32, reason: String },
}

pub(in crate::entity_builder) async fn resolve_kept(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    prepared: &PreparedEntity,
    mention: &str,
    known: &[KnownEntity],
    decider: &dyn EntityDecider,
) -> Result<Option<Kept>, WorkerError> {
    let mut prepared = prepared.clone();
    let suggested = match ref_gate(&prepared, known) {
        RefGate::Direct(subject) => {
            rewrite::record_verified_ref();
            let (prepared, merges) =
                accept_existing(decider, prepared, mention, known, &subject).await;
            let prepared = apply_merges(env, prepared, merges).await?;
            return Ok(Some(Kept {
                prepared,
                embedding: None,
            }));
        }
        RefGate::Match { suggested } => suggested,
    };
    if prepared.entity_ref.is_some() {
        let detail = suggested.as_ref().map_or_else(
            || "unknown entity".to_owned(),
            |item| format!("{} {}", item.name, item.id),
        );
        rewrite::record_redirected(&prepared.name, &detail);
        prepared.entity_ref = None;
    }
    let embedding = embed_observation(env, &prepared).await;
    let mut ranked = candidates(env, &prepared, embedding.as_deref()).await?;
    inject_suggested(&mut ranked, suggested);
    let ranked = offered(&prepared, &ranked);
    let decision = decision_for(decider, &prepared, &ranked).await;
    match interpret(&prepared, &ranked, decision, env.settings.threshold) {
        MatchStep::Ready(mut bound) => {
            let merges = if let Some(id) = bound.entity_ref
                && let Some(subject) = subject_for(id, &ranked, known)
            {
                let (next, merges) =
                    accept_existing(decider, bound, mention, known, &subject).await;
                bound = next;
                merges
            } else {
                bound.aliases = new_entity_aliases(&bound.name, &bound.aliases, mention, known);
                Vec::new()
            };
            let prepared = apply_merges(env, bound, merges).await?;
            Ok(Some(Kept {
                prepared,
                embedding,
            }))
        }
        MatchStep::Held { confidence, reason } => {
            env.store
                .hold_candidate(summary, &prepared, confidence, &reason, env.now)
                .await?;
            Ok(None)
        }
    }
}

async fn candidates(
    env: &EntityBuilderEnv<'_>,
    prepared: &PreparedEntity,
    embedding: Option<&[f32]>,
) -> Result<Vec<MatchCandidate>, WorkerError> {
    let found = env
        .store
        .match_candidates(&CandidateQuery {
            names: query_names(prepared),
            type_name: prepared.type_name.clone(),
            embedding: embedding.map(<[f32]>::to_vec),
        })
        .await?;
    Ok(rank_candidates(found))
}

pub(super) async fn decision_for(
    decider: &dyn EntityDecider,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
) -> Result<RawDecision, ()> {
    if ranked.is_empty()
        || exact_id(
            &prepared.type_name,
            &prepared.name,
            &prepared.aliases,
            ranked,
        )
        .is_some()
    {
        return Err(());
    }
    ask(decider, &request_for(prepared, ranked))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "entity match stayed undecided");
        })
}

pub(super) async fn ask(
    decider: &dyn EntityDecider,
    request: &DecideRequest,
) -> Result<RawDecision, WorkerError> {
    match decider.decide(request).await {
        Ok(decision) => Ok(decision),
        Err(error) => {
            tracing::warn!(%error, "entity match decide failed; retrying once");
            decider.decide(request).await
        }
    }
}

pub(super) fn interpret(
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    decision: Result<RawDecision, ()>,
    threshold: f32,
) -> MatchStep {
    if let Some(id) = exact_id(
        &prepared.type_name,
        &prepared.name,
        &prepared.aliases,
        ranked,
    ) {
        return MatchStep::Ready(bound(prepared, id));
    }
    if ranked.is_empty() {
        return MatchStep::Ready(prepared.clone());
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a failed decision holds the entity"
        )
    )]
    let Ok(decision) = decision else {
        return MatchStep::Held {
            confidence: 0.0,
            reason: "decide-error".to_owned(),
        };
    };
    let act = gate(
        &decision.choice,
        decision.confidence,
        threshold,
        ranked.len(),
    );
    match act {
        MatchAct::New => {
            note_new_cross(prepared, ranked);
            MatchStep::Ready(prepared.clone())
        }
        MatchAct::Hold => MatchStep::Held {
            confidence: decision.confidence,
            reason: "low-confidence".to_owned(),
        },
        MatchAct::Existing(index) => match chosen_id(&prepared.type_name, ranked, act) {
            Some(id) => MatchStep::Ready(bound(prepared, id)),
            None => bind_cross(prepared, ranked, index, decision.confidence),
        },
    }
}

fn bind_cross(
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    index: usize,
    confidence: f32,
) -> MatchStep {
    let Some(candidate) = ranked.get(index) else {
        return MatchStep::Held {
            confidence,
            reason: "cross-type".to_owned(),
        };
    };
    if !names_same_entity(&prepared.type_name, &candidate.type_name)
        || !name_identifies(&prepared.name, &candidate.name, &candidate.aliases)
    {
        return MatchStep::Held {
            confidence,
            reason: "cross-type".to_owned(),
        };
    }
    tracing::info!(
        target: "entity_match",
        name = %prepared.name,
        observed_type = %prepared.type_name,
        kept_name = %candidate.name,
        kept_type = %candidate.type_name,
        "cross-type bind"
    );
    MatchStep::Ready(bound(prepared, candidate.id))
}

fn note_new_cross(prepared: &PreparedEntity, ranked: &[MatchCandidate]) {
    for candidate in ranked {
        if types_compatible(&prepared.type_name, &candidate.type_name) {
            continue;
        }
        tracing::info!(
            target: "entity_match",
            name = %prepared.name,
            observed_type = %prepared.type_name,
            candidate = %candidate.name,
            candidate_type = %candidate.type_name,
            "cross-type new"
        );
    }
}

pub(super) fn offered(prepared: &PreparedEntity, ranked: &[MatchCandidate]) -> Vec<MatchCandidate> {
    ranked
        .iter()
        .filter(|candidate| {
            types_compatible(&prepared.type_name, &candidate.type_name)
                || (names_same_entity(&prepared.type_name, &candidate.type_name)
                    && name_identifies(&prepared.name, &candidate.name, &candidate.aliases))
        })
        .cloned()
        .collect()
}

fn bound(prepared: &PreparedEntity, id: Uuid) -> PreparedEntity {
    let mut prepared = prepared.clone();
    prepared.entity_ref = Some(id);
    prepared
}

fn request_for(prepared: &PreparedEntity, ranked: &[MatchCandidate]) -> DecideRequest {
    DecideRequest {
        observation: observation_text(prepared),
        options: ranked.iter().map(candidate_line).collect(),
    }
}

fn observation_text(prepared: &PreparedEntity) -> String {
    format!(
        "name: {}\ntype: {}\naliases: {}\nfacts: {}",
        prepared.name,
        prepared.type_name,
        prepared.aliases.join(", "),
        prepared_facts(prepared)
    )
}

fn candidate_line(candidate: &MatchCandidate) -> String {
    let aliases = candidate.aliases.join(", ");
    let summary = candidate.summary.trim();
    if summary.is_empty() {
        format!(
            "{} ({}) aliases: {aliases}",
            candidate.name, candidate.type_name
        )
    } else {
        format!(
            "{} ({}) aliases: {aliases} — {summary}",
            candidate.name, candidate.type_name
        )
    }
}

fn query_names(prepared: &PreparedEntity) -> Vec<String> {
    let mut names = vec![prepared.name.clone()];
    names.extend(prepared.aliases.iter().cloned());
    names.retain(|name| !name.trim().is_empty());
    names
}

pub(super) fn prepared_facts(prepared: &PreparedEntity) -> String {
    prepared
        .facts
        .iter()
        .map(|fact| format!("{}: {}", fact.attribute, fact.value))
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn fact_line(view: &EntityView) -> String {
    view.facts
        .iter()
        .map(|fact| fact.value.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

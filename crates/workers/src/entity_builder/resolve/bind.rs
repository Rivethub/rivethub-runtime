use kernel::wiki::entity_match::MatchAct;
use kernel::wiki::entity_match::canonical_key;
use kernel::wiki::entity_match::chosen_id;
use kernel::wiki::entity_match::exact_id;
use kernel::wiki::entity_match::gate;
use kernel::wiki::entity_match::names_same_entity;
use kernel::wiki::entity_match::rank_candidates;
use kernel::wiki::entity_match::surface_markers;
use kernel::wiki::entity_match::types_compatible;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::MatchDecision;
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
    let asked = decision_for(decider, &prepared, &ranked).await;
    let (choice, confidence) = asked_fields(&asked);
    let forced = forces_alias(&choice, confidence, &ranked, env.settings.threshold);
    let backend = decider.backend_name().to_owned();
    let observed_name = prepared.name.clone();
    match apply_asked(&prepared, &ranked, asked, env.settings.threshold) {
        Err(error) => fail_undecided(env, summary, &prepared, &ranked, &backend, error).await,
        Ok(MatchStep::Held {
            confidence: held_confidence,
            reason,
        }) => {
            park_held(
                env,
                summary,
                &prepared,
                &ranked,
                &reason,
                Ruling {
                    verdict: "hold",
                    choice: &choice,
                    confidence: held_confidence,
                    model: &backend,
                    entity_id: None,
                },
            )
            .await
        }
        Ok(MatchStep::Ready(bound)) => {
            keep_ready(
                env,
                summary,
                decider,
                known,
                mention,
                Ready {
                    bound,
                    ranked: &ranked,
                    choice: &choice,
                    confidence,
                    forced,
                    backend: &backend,
                    observed_name: &observed_name,
                    embedding,
                },
            )
            .await
        }
    }
}

pub(super) struct Ruling<'a> {
    pub(super) verdict: &'a str,
    pub(super) choice: &'a str,
    pub(super) confidence: f32,
    pub(super) model: &'a str,
    pub(super) entity_id: Option<Uuid>,
}

struct Ready<'a> {
    bound: PreparedEntity,
    ranked: &'a [MatchCandidate],
    choice: &'a str,
    confidence: f32,
    forced: bool,
    backend: &'a str,
    observed_name: &'a str,
    embedding: Option<Vec<f32>>,
}

async fn fail_undecided(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    backend: &str,
    error: WorkerError,
) -> Result<Option<Kept>, WorkerError> {
    note_entity(
        env,
        summary,
        prepared,
        ranked,
        &Ruling {
            verdict: "unreachable",
            choice: "",
            confidence: 0.0,
            model: backend,
            entity_id: None,
        },
    )
    .await?;
    Err(undecided(&error))
}

async fn park_held(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    reason: &str,
    verdict: Ruling<'_>,
) -> Result<Option<Kept>, WorkerError> {
    let confidence = verdict.confidence;
    note_entity(env, summary, prepared, ranked, &verdict).await?;
    env.store
        .hold_candidate(summary, prepared, confidence, reason, env.now)
        .await?;
    Ok(None)
}

async fn keep_ready(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    decider: &dyn EntityDecider,
    known: &[KnownEntity],
    mention: &str,
    mut ready: Ready<'_>,
) -> Result<Option<Kept>, WorkerError> {
    let (verdict, model) = recorded(
        ready.bound.entity_ref.is_some(),
        ready.ranked.is_empty(),
        ready.forced,
        ready.backend,
    );
    let kept_confidence = if verdict == "same" && !ready.forced {
        1.0
    } else {
        ready.confidence
    };
    note_entity(
        env,
        summary,
        &ready.bound,
        ready.ranked,
        &Ruling {
            verdict: &verdict,
            choice: ready.choice,
            confidence: kept_confidence,
            model: &model,
            entity_id: ready.bound.entity_ref,
        },
    )
    .await?;
    let merges = if let Some(id) = ready.bound.entity_ref
        && let Some(subject) = subject_for(id, ready.ranked, known)
    {
        let (next, merges) = accept_existing(decider, ready.bound, mention, known, &subject).await;
        ready.bound = next;
        if ready.forced {
            keep_observed_name(&mut ready.bound, ready.observed_name);
        }
        merges
    } else {
        ready.bound.aliases =
            new_entity_aliases(&ready.bound.name, &ready.bound.aliases, mention, known);
        Vec::new()
    };
    let prepared = apply_merges(env, ready.bound, merges).await?;
    Ok(Some(Kept {
        prepared,
        embedding: ready.embedding,
    }))
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
            markers: query_markers(prepared),
        })
        .await?;
    Ok(rank_candidates(found))
}

pub(super) enum Asked {
    Skip,
    Answer(RawDecision),
    Unreachable(WorkerError),
}

pub(super) async fn decision_for(
    decider: &dyn EntityDecider,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
) -> Asked {
    if ranked.is_empty()
        || exact_id(
            &prepared.type_name,
            &prepared.name,
            &prepared.aliases,
            ranked,
        )
        .is_some()
    {
        return Asked::Skip;
    }
    match ask(decider, &request_for(prepared, ranked)).await {
        Ok(decision) => Asked::Answer(decision),
        Err(error) => {
            tracing::warn!(%error, "entity match stayed undecided");
            Asked::Unreachable(error)
        }
    }
}

pub(super) fn apply_asked(
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    asked: Asked,
    threshold: f32,
) -> Result<MatchStep, WorkerError> {
    let decision = match asked {
        Asked::Unreachable(error) => return Err(error),
        Asked::Skip => Err(()),
        Asked::Answer(decision) => Ok(decision),
    };
    Ok(interpret(prepared, ranked, decision, threshold))
}

pub(super) fn undecided(error: &WorkerError) -> WorkerError {
    WorkerError::Llm {
        message: format!("entity match undecided: {error}"),
    }
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
    if !names_same_entity(&prepared.type_name, &candidate.type_name) {
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
        .filter(|candidate| names_same_entity(&prepared.type_name, &candidate.type_name))
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

fn query_markers(prepared: &PreparedEntity) -> Vec<String> {
    let mut texts = query_names(prepared);
    texts.extend(prepared.facts.iter().map(|fact| fact.value.clone()));
    surface_markers(&texts)
}

fn asked_fields(asked: &Asked) -> (String, f32) {
    match asked {
        Asked::Answer(decision) => (decision.choice.clone(), decision.confidence),
        Asked::Skip | Asked::Unreachable(_) => (String::new(), 0.0),
    }
}

fn forces_alias(choice: &str, confidence: f32, ranked: &[MatchCandidate], threshold: f32) -> bool {
    matches!(
        gate(choice, confidence, threshold, ranked.len()),
        MatchAct::Existing(_)
    )
}

fn recorded(bound: bool, ranked_empty: bool, forced: bool, backend: &str) -> (String, String) {
    if ranked_empty {
        ("none".to_owned(), "none".to_owned())
    } else if bound && !forced {
        ("same".to_owned(), "exact".to_owned())
    } else if bound {
        ("same".to_owned(), backend.to_owned())
    } else {
        ("different".to_owned(), backend.to_owned())
    }
}

pub(super) fn entity_decision(
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    verdict: &Ruling<'_>,
) -> MatchDecision {
    MatchDecision {
        kind: "entity".to_owned(),
        proposed_name: prepared.name.clone(),
        proposed_type: prepared.type_name.clone(),
        candidates: ranked.iter().map(candidate_label).collect(),
        verdict: verdict.verdict.to_owned(),
        choice: verdict.choice.to_owned(),
        confidence: verdict.confidence,
        model: verdict.model.to_owned(),
        entity_id: verdict.entity_id,
    }
}

fn candidate_label(candidate: &MatchCandidate) -> String {
    format!(
        "{} ({}) {}",
        candidate.name, candidate.type_name, candidate.id
    )
}

async fn note_entity(
    env: &EntityBuilderEnv<'_>,
    summary: Uuid,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
    verdict: &Ruling<'_>,
) -> Result<(), WorkerError> {
    let decision = entity_decision(prepared, ranked, verdict);
    env.store
        .record_decision(summary, &decision, env.now)
        .await?;
    Ok(())
}

fn keep_observed_name(bound: &mut PreparedEntity, observed: &str) {
    let key = canonical_key(observed);
    if key.is_empty() || key == canonical_key(&bound.name) {
        return;
    }
    if bound
        .aliases
        .iter()
        .any(|alias| canonical_key(alias) == key)
    {
        return;
    }
    bound.aliases.push(observed.to_owned());
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

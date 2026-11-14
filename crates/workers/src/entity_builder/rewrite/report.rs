use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::FactReconciliation;
use serde::Serialize;

use super::super::gather;
use crate::WorkerError;

const EXAMPLE_LIMIT: usize = 5;
const ALIAS_EXAMPLE_LIMIT: usize = 10;
const EXAMPLE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FactExample {
    pub entity: String,
    pub attribute: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedExample {
    pub entity: String,
    pub attribute: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FactCarryReport {
    pub rewrites_with_carry_forward: u64,
    pub facts_carried: u64,
    pub facts_retired: u64,
    pub retirements_rejected: u64,
    pub carried_examples: Vec<FactExample>,
    pub retired_examples: Vec<FactExample>,
    pub rejected_examples: Vec<RejectedExample>,
}

static LEDGER: parking_lot::Mutex<FactCarryReport> = parking_lot::Mutex::new(FactCarryReport {
    rewrites_with_carry_forward: 0,
    facts_carried: 0,
    facts_retired: 0,
    retirements_rejected: 0,
    carried_examples: Vec::new(),
    retired_examples: Vec::new(),
    rejected_examples: Vec::new(),
});

#[must_use]
pub fn fact_carry_report() -> FactCarryReport {
    LEDGER.lock().clone()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityExample {
    pub entity: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeExample {
    pub from_name: String,
    pub into_name: String,
    pub probability: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EntityRunReport {
    pub no_evidence_skips: u64,
    pub empty_rewrites: u64,
    pub entity_errors: u64,
    pub no_evidence_examples: Vec<EntityExample>,
    pub empty_rewrite_examples: Vec<EntityExample>,
    pub entity_error_examples: Vec<EntityExample>,
    pub passage_chars: u64,
    pub excerpted_messages: u64,
    pub excerpt_chars: u64,
    pub excerpt_source_chars: u64,
    pub verified_refs: u64,
    pub redirected_refs: u64,
    pub aliases_accepted: u64,
    pub aliases_rejected: u64,
    pub aliases_paired: u64,
    pub merges: u64,
    pub redirected_examples: Vec<EntityExample>,
    pub alias_rejected_examples: Vec<EntityExample>,
    pub merge_examples: Vec<MergeExample>,
}

impl EntityRunReport {
    #[must_use]
    pub fn excerpt_ratio(&self) -> String {
        if self.excerpt_source_chars == 0 {
            return "0".to_owned();
        }
        let scaled = u128::from(self.excerpt_chars).saturating_mul(10_000)
            / u128::from(self.excerpt_source_chars);
        let whole = scaled / 10_000;
        let part = scaled % 10_000;
        format!("{whole}.{part:04}")
    }
}

static OUTCOMES: parking_lot::Mutex<EntityRunReport> = parking_lot::Mutex::new(EntityRunReport {
    no_evidence_skips: 0,
    empty_rewrites: 0,
    entity_errors: 0,
    no_evidence_examples: Vec::new(),
    empty_rewrite_examples: Vec::new(),
    entity_error_examples: Vec::new(),
    passage_chars: 0,
    excerpted_messages: 0,
    excerpt_chars: 0,
    excerpt_source_chars: 0,
    verified_refs: 0,
    redirected_refs: 0,
    aliases_accepted: 0,
    aliases_rejected: 0,
    aliases_paired: 0,
    merges: 0,
    redirected_examples: Vec::new(),
    alias_rejected_examples: Vec::new(),
    merge_examples: Vec::new(),
});

#[must_use]
pub fn entity_run_report() -> EntityRunReport {
    let mut report = OUTCOMES.lock().clone();
    let passages = gather::passage_report();
    report.passage_chars = passages.passage_chars;
    report.excerpted_messages = passages.excerpted_messages;
    report.excerpt_chars = passages.excerpt_chars;
    report.excerpt_source_chars = passages.excerpt_source_chars;
    report
}

pub(in crate::entity_builder) fn record_verified_ref() {
    let mut report = OUTCOMES.lock();
    report.verified_refs = report.verified_refs.saturating_add(1);
}

pub(in crate::entity_builder) fn record_redirected(name: &str, detail: &str) {
    let mut report = OUTCOMES.lock();
    report.redirected_refs = report.redirected_refs.saturating_add(1);
    push_entity(&mut report.redirected_examples, name, detail);
}

pub(in crate::entity_builder) fn record_alias_accepted() {
    let mut report = OUTCOMES.lock();
    report.aliases_accepted = report.aliases_accepted.saturating_add(1);
}

pub(in crate::entity_builder) fn record_alias_paired() {
    let mut report = OUTCOMES.lock();
    report.aliases_paired = report.aliases_paired.saturating_add(1);
}

pub(in crate::entity_builder) fn record_merged(from: &str, into: &str, score: f32) {
    let shown = if score.is_finite() { score } else { 0.0 };
    let example = MergeExample {
        from_name: one_line(from),
        into_name: one_line(into),
        probability: format!("{shown:.2}"),
    };
    let mut report = OUTCOMES.lock();
    report.merges = report.merges.saturating_add(1);
    if report.merge_examples.len() < ALIAS_EXAMPLE_LIMIT {
        report.merge_examples.push(example);
    }
}

pub(in crate::entity_builder) fn record_alias_rejected(entity: &str, alias: &str, reason: &str) {
    let detail = format!("{alias} {reason}");
    let mut report = OUTCOMES.lock();
    report.aliases_rejected = report.aliases_rejected.saturating_add(1);
    push_entity_limited(
        &mut report.alias_rejected_examples,
        entity,
        &detail,
        ALIAS_EXAMPLE_LIMIT,
    );
}

pub(super) fn record_no_evidence(entity: &str) {
    tracing::info!(
        entity,
        "extract-entities: skipped an entity with no passage"
    );
    let mut report = OUTCOMES.lock();
    report.no_evidence_skips = report.no_evidence_skips.saturating_add(1);
    push_entity(&mut report.no_evidence_examples, entity, "no passage");
}

pub(super) fn record_empty(entity: &str, fresh: bool) {
    let detail = if fresh {
        "empty rewrite dropped"
    } else {
        "empty rewrite kept the page"
    };
    tracing::info!(entity, fresh, "extract-entities: empty rewrite");
    let mut report = OUTCOMES.lock();
    report.empty_rewrites = report.empty_rewrites.saturating_add(1);
    push_entity(&mut report.empty_rewrite_examples, entity, detail);
}

pub(super) fn record_entity_error(entity: &str, detail: &str) {
    let mut report = OUTCOMES.lock();
    report.entity_errors = report.entity_errors.saturating_add(1);
    push_entity(&mut report.entity_error_examples, entity, detail);
}

fn push_entity(examples: &mut Vec<EntityExample>, entity: &str, detail: &str) {
    push_entity_limited(examples, entity, detail, EXAMPLE_LIMIT);
}

fn push_entity_limited(
    examples: &mut Vec<EntityExample>,
    entity: &str,
    detail: &str,
    limit: usize,
) {
    if examples.len() >= limit {
        return;
    }
    examples.push(EntityExample {
        entity: entity.to_owned(),
        detail: one_line(detail).chars().take(EXAMPLE_CHARS).collect(),
    });
}

pub(super) fn record_reconciliation(entity: &str, reconciled: &FactReconciliation) {
    let carried_forward = reconciled.carried.len();
    let retired = reconciled.retired.len();
    let retirements_rejected = reconciled.rejected.len();
    tracing::info!(
        carried_forward,
        retired,
        retirements_rejected,
        entity,
        "entity rewrite reconciled current facts"
    );
    let mut ledger = LEDGER.lock();
    ledger.facts_carried = ledger
        .facts_carried
        .saturating_add(count_of(carried_forward));
    ledger.facts_retired = ledger.facts_retired.saturating_add(count_of(retired));
    ledger.retirements_rejected = ledger
        .retirements_rejected
        .saturating_add(count_of(retirements_rejected));
    if carried_forward > 0 {
        ledger.rewrites_with_carry_forward = ledger.rewrites_with_carry_forward.saturating_add(1);
    }
    for fact in &reconciled.carried {
        push_example(
            &mut ledger.carried_examples,
            entity,
            &fact.attribute,
            &fact.value,
        );
    }
    for item in &reconciled.retired {
        push_example(
            &mut ledger.retired_examples,
            entity,
            &item.attribute,
            &item.reason(),
        );
    }
    for item in &reconciled.rejected {
        push_rejected(
            &mut ledger.rejected_examples,
            entity,
            &item.attribute,
            &item.kind,
            &item.detail,
        );
    }
}

fn push_example(examples: &mut Vec<FactExample>, entity: &str, attribute: &str, detail: &str) {
    if examples.len() >= EXAMPLE_LIMIT {
        return;
    }
    examples.push(FactExample {
        entity: entity.to_owned(),
        attribute: attribute.to_owned(),
        detail: one_line(detail),
    });
}

fn push_rejected(
    examples: &mut Vec<RejectedExample>,
    entity: &str,
    attribute: &str,
    kind: &str,
    detail: &str,
) {
    if examples.len() >= EXAMPLE_LIMIT {
        return;
    }
    examples.push(RejectedExample {
        entity: entity.to_owned(),
        attribute: one_line(attribute),
        kind: one_line(kind),
        detail: one_line(detail).chars().take(EXAMPLE_CHARS).collect(),
    });
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn count_of(len: usize) -> u64 {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: an example count that does not fit saturates"
        )
    )]
    let count = u64::try_from(len).unwrap_or(u64::MAX);
    count
}

pub(super) fn owner_of(view: &EntityView, leaves: &[EntityLeaf]) -> Result<String, WorkerError> {
    let owner = view
        .owner
        .clone()
        .or_else(|| leaves.iter().find_map(|leaf| leaf.owner.clone()))
        .unwrap_or_default();
    if owner.trim().is_empty() {
        return Err(WorkerError::Llm {
            message: format!("entity {} has no owner", view.slug),
        });
    }
    Ok(owner)
}

use kernel::wiki::entity_admit::address_tied_to_name;
use kernel::wiki::entity_admit::alias_claims_known;
use kernel::wiki::entity_admit::alias_in_evidence;
use kernel::wiki::entity_admit::alias_is_address;
use kernel::wiki::entity_admit::identifying_aliases;
use kernel::wiki::entity_admit::merge_partner;
use kernel::wiki::entity_admit::names_share_mention;
use kernel::wiki::entity_match::canonical_key;
use kernel::wiki::entity_match::ref_verified;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::PreparedEntity;
use uuid::Uuid;

use super::super::EntityBuilderEnv;
use super::super::decide::ALIAS_MIN;
use super::super::decide::EntityDecider;
use super::super::decide::MERGE_MIN;
use super::super::rewrite;
use crate::WorkerError;

pub(super) struct AliasSubject {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) type_name: String,
    pub(super) summary: String,
    pub(super) aliases: Vec<String>,
}

pub(super) enum RefGate {
    Direct(AliasSubject),
    Match { suggested: Option<MatchCandidate> },
}

pub(super) fn ref_gate(prepared: &PreparedEntity, known: &[KnownEntity]) -> RefGate {
    let Some(id) = prepared.entity_ref else {
        return RefGate::Match { suggested: None };
    };
    let Some(entity) = known.iter().find(|item| item.id == id) else {
        return RefGate::Match { suggested: None };
    };
    if ref_verified(&prepared.name, &entity.name, &entity.aliases) {
        RefGate::Direct(subject_from_known(entity))
    } else {
        RefGate::Match {
            suggested: Some(candidate_from_known(entity)),
        }
    }
}

fn subject_from_known(entity: &KnownEntity) -> AliasSubject {
    AliasSubject {
        id: entity.id,
        name: entity.name.clone(),
        type_name: entity.type_name.clone(),
        summary: one_line(entity.facts.first().map_or("", |fact| fact.value.as_str())),
        aliases: entity.aliases.clone(),
    }
}

fn subject_from_candidate(candidate: &MatchCandidate) -> AliasSubject {
    AliasSubject {
        id: candidate.id,
        name: candidate.name.clone(),
        type_name: candidate.type_name.clone(),
        summary: one_line(&candidate.summary),
        aliases: candidate.aliases.clone(),
    }
}

fn candidate_from_known(entity: &KnownEntity) -> MatchCandidate {
    let subject = subject_from_known(entity);
    let summary = if subject.summary == "none" {
        String::new()
    } else {
        subject.summary
    };
    MatchCandidate {
        id: subject.id,
        name: subject.name,
        type_name: subject.type_name,
        aliases: subject.aliases,
        summary,
        trigram: 1.0,
        embedding: 0.0,
    }
}

pub(super) fn subject_for(
    id: Uuid,
    ranked: &[MatchCandidate],
    known: &[KnownEntity],
) -> Option<AliasSubject> {
    if let Some(candidate) = ranked.iter().find(|item| item.id == id) {
        return Some(subject_from_candidate(candidate));
    }
    known
        .iter()
        .find(|item| item.id == id)
        .map(subject_from_known)
}

pub(super) fn inject_suggested(
    ranked: &mut Vec<MatchCandidate>,
    suggested: Option<MatchCandidate>,
) {
    let Some(extra) = suggested else {
        return;
    };
    if ranked.iter().any(|item| item.id == extra.id) {
        return;
    }
    ranked.push(extra);
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat: String = flat.chars().take(160).collect();
    if flat.is_empty() {
        "none".to_owned()
    } else {
        flat
    }
}

fn alias_question(alias: &str, subject: &AliasSubject, mention: &str) -> String {
    let known_as = if subject.aliases.is_empty() {
        "none".to_owned()
    } else {
        subject.aliases.join(", ")
    };
    format!(
        "Transcript: \"{mention}\". Entity: {} ({}; also known as {known_as}; {}). Does \"{alias}\" in this transcript refer to this same entity?",
        subject.name.trim(),
        subject.type_name.trim(),
        subject.summary.trim()
    )
}

const PAIR_CHARS: usize = 30;

#[derive(Clone, Copy)]
enum PairLink {
    NameThenAlias,
    AliasThenName,
}

pub(super) fn alias_paired(name: &str, alias: &str, mention: &str) -> bool {
    let name = name.trim().to_lowercase();
    let alias = alias.trim().to_lowercase();
    if name.is_empty() || alias.is_empty() || name == alias {
        return false;
    }
    let haystack = mention.to_lowercase();
    paired_order(&haystack, &name, &alias, PairLink::NameThenAlias)
        || paired_order(&haystack, &alias, &name, PairLink::AliasThenName)
}

fn paired_order(haystack: &str, left: &str, right: &str, link: PairLink) -> bool {
    let mut from = 0;
    while let Some(start) = bounded_at(haystack, left, from) {
        let end = start.saturating_add(left.len());
        if let Some(after) = haystack.get(end..)
            && let Some(offset) = link_at(after, right, link)
            && let Some(between) = after.get(..offset)
            && between.chars().count() <= PAIR_CHARS
        {
            return true;
        }
        let Some(step) = haystack.get(start..).and_then(|tail| tail.chars().next()) else {
            return false;
        };
        let next = start.saturating_add(step.len_utf8());
        if next <= start {
            return false;
        }
        from = next;
    }
    false
}

fn bounded_at(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut from = from;
    while from < haystack.len() {
        let tail = haystack.get(from..)?;
        let rel = tail.find(needle)?;
        let start = from.saturating_add(rel);
        let end = start.saturating_add(needle.len());
        if edges_free(haystack, start, end) {
            return Some(start);
        }
        let step = haystack
            .get(start..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8);
        let next = start.saturating_add(step);
        if next <= start {
            return None;
        }
        from = next;
    }
    None
}

fn edges_free(haystack: &str, start: usize, end: usize) -> bool {
    let before = haystack
        .get(..start)
        .and_then(|prefix| prefix.chars().next_back());
    let after = haystack.get(end..).and_then(|suffix| suffix.chars().next());
    before.is_none_or(is_boundary) && after.is_none_or(is_boundary)
}

fn link_at(after: &str, right: &str, link: PairLink) -> Option<usize> {
    match link {
        PairLink::NameThenAlias => symbol_then(after, '(', right)
            .or_else(|| symbol_then(after, '/', right))
            .or_else(|| symbol_then(after, '=', right))
            .or_else(|| word_then(after, "aka", right)),
        PairLink::AliasThenName => {
            symbol_then(after, '(', right).or_else(|| word_then(after, "is", right))
        }
    }
}

fn symbol_then(after: &str, symbol: char, right: &str) -> Option<usize> {
    let rest = after.trim_start();
    let lead = after.len().checked_sub(rest.len())?;
    let rest = rest.strip_prefix(symbol)?;
    let body = rest.trim_start();
    let gap = rest.len().checked_sub(body.len())?;
    if !token_at(body, right) {
        return None;
    }
    Some(lead.saturating_add(symbol.len_utf8()).saturating_add(gap))
}

fn word_then(after: &str, word: &str, right: &str) -> Option<usize> {
    let rest = after.trim_start();
    if rest.len() == after.len() {
        return None;
    }
    let lead = after.len().saturating_sub(rest.len());
    let rest = rest.strip_prefix(word)?;
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    let body = rest.trim_start();
    if body.len() == rest.len() {
        return None;
    }
    let gap = rest.len().saturating_sub(body.len());
    if !token_at(body, right) {
        return None;
    }
    Some(lead.saturating_add(word.len()).saturating_add(gap))
}

fn token_at(text: &str, token: &str) -> bool {
    let Some(rest) = text.strip_prefix(token) else {
        return false;
    };
    rest.chars().next().is_none_or(is_boundary)
}

fn is_boundary(ch: char) -> bool {
    !ch.is_alphanumeric()
}

pub(super) async fn accept_existing(
    decider: &dyn EntityDecider,
    mut prepared: PreparedEntity,
    mention: &str,
    known: &[KnownEntity],
    subject: &AliasSubject,
) -> (PreparedEntity, Vec<ConfirmedMerge>) {
    let proposed = proposed_aliases(&prepared, subject);
    let filtered = filter_new_aliases(decider, proposed, subject, mention, known).await;
    prepared.aliases = filtered.kept;
    prepared.name.clone_from(&subject.name);
    prepared.entity_ref = Some(subject.id);
    (prepared, filtered.merges)
}

pub(super) async fn apply_merges(
    env: &EntityBuilderEnv<'_>,
    mut prepared: PreparedEntity,
    merges: Vec<ConfirmedMerge>,
) -> Result<PreparedEntity, WorkerError> {
    let mut current = prepared.entity_ref;
    let mut name = prepared.name.clone();
    for merge in merges {
        let Some(left) = current else {
            break;
        };
        let done = match env
            .store
            .merge_entities(left, merge.other_id, env.now)
            .await
        {
            Ok(Some(done)) => done,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, into = %left, from = %merge.other_id, "entity merge failed; kept both");
                continue;
            }
        };
        if done.performed {
            rewrite::record_merged(&done.absorbed_name, &done.survivor_name, merge.score);
        }
        current = Some(done.survivor);
        name = done.survivor_name;
    }
    prepared.entity_ref = current;
    prepared.name = name;
    Ok(prepared)
}

fn proposed_aliases(prepared: &PreparedEntity, subject: &AliasSubject) -> Vec<String> {
    let mut names = prepared.aliases.clone();
    for fact in &prepared.facts {
        names.extend(identifying_aliases(&prepared.type_name, &fact.value));
    }
    let key = canonical_key(&prepared.name);
    let already = key == canonical_key(&subject.name)
        || subject
            .aliases
            .iter()
            .any(|alias| canonical_key(alias) == key);
    if !key.is_empty() && !already {
        names.push(prepared.name.clone());
    }
    names
}

pub(super) struct AliasFilter {
    pub(super) kept: Vec<String>,
    pub(super) merges: Vec<ConfirmedMerge>,
}

pub(super) struct ConfirmedMerge {
    pub(super) other_id: Uuid,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the partner name is asserted by the merge detection tests"
        )
    )]
    pub(super) other_name: String,
    pub(super) score: f32,
}

enum KnownAlias {
    Continue,
    Stop,
    Merge(ConfirmedMerge),
}

pub(super) async fn filter_new_aliases(
    decider: &dyn EntityDecider,
    proposed: Vec<String>,
    subject: &AliasSubject,
    mention: &str,
    known: &[KnownEntity],
) -> AliasFilter {
    let mut kept = Vec::new();
    let mut merges = Vec::new();
    for alias in proposed {
        let trimmed = alias.trim();
        let key = canonical_key(trimmed);
        if key.is_empty() || key == canonical_key(&subject.name) {
            continue;
        }
        if subject
            .aliases
            .iter()
            .any(|have| canonical_key(have) == key)
            || kept.iter().any(|have: &String| canonical_key(have) == key)
        {
            continue;
        }
        match known_alias(decider, trimmed, subject, mention, known).await {
            KnownAlias::Stop => continue,
            KnownAlias::Merge(merge) => {
                if merges
                    .iter()
                    .all(|item: &ConfirmedMerge| item.other_id != merge.other_id)
                {
                    merges.push(merge);
                }
                continue;
            }
            KnownAlias::Continue => {}
        }
        if let Some(name) = plain_alias(decider, trimmed, subject, mention).await {
            kept.push(name);
        }
    }
    AliasFilter { kept, merges }
}

async fn known_alias(
    decider: &dyn EntityDecider,
    alias: &str,
    subject: &AliasSubject,
    mention: &str,
    known: &[KnownEntity],
) -> KnownAlias {
    let Some(other) = merge_partner(alias, subject.id, &subject.type_name, known) else {
        return claimed_name(alias, subject, known);
    };
    let tied = mention_ties(&subject.name, &other.name, mention)
        || mention_ties(&subject.name, alias, mention);
    if !tied {
        return claimed_name(alias, subject, known);
    }
    confirm_merge(decider, alias, subject, other, mention).await
}

fn claimed_name(alias: &str, subject: &AliasSubject, known: &[KnownEntity]) -> KnownAlias {
    if alias_claims_known(alias, &subject.name, known) {
        rewrite::record_alias_rejected(&subject.name, alias, "another entity's name");
        KnownAlias::Stop
    } else {
        KnownAlias::Continue
    }
}

fn mention_ties(left: &str, right: &str, mention: &str) -> bool {
    alias_paired(left, right, mention) || names_share_mention(left, right, mention)
}

async fn confirm_merge(
    decider: &dyn EntityDecider,
    alias: &str,
    subject: &AliasSubject,
    other: &KnownEntity,
    mention: &str,
) -> KnownAlias {
    let question = merge_question(mention, subject, other);
    match ask_alias(decider, &question).await {
        Ok(score) if score.is_finite() && score >= MERGE_MIN => KnownAlias::Merge(ConfirmedMerge {
            other_id: other.id,
            other_name: other.name.clone(),
            score,
        }),
        Ok(score) => {
            let shown = if score.is_finite() { score } else { 0.0 };
            rewrite::record_alias_rejected(
                &subject.name,
                alias,
                &format!("merge p={shown:.2} below {MERGE_MIN:.1}"),
            );
            KnownAlias::Stop
        }
        Err(error) => {
            tracing::warn!(%error, alias, "merge check failed");
            rewrite::record_alias_rejected(&subject.name, alias, "unchecked");
            KnownAlias::Stop
        }
    }
}

fn merge_question(mention: &str, subject: &AliasSubject, other: &KnownEntity) -> String {
    format!(
        "Transcript: \"{mention}\". Entity A: {}. Entity B: {}. Are these the same real-world thing?",
        entity_blurb(
            &subject.name,
            &subject.type_name,
            &subject.aliases,
            &subject.summary
        ),
        entity_blurb(
            &other.name,
            &other.type_name,
            &other.aliases,
            &known_summary(other)
        ),
    )
}

fn entity_blurb(name: &str, type_name: &str, aliases: &[String], summary: &str) -> String {
    let known_as = if aliases.is_empty() {
        "none".to_owned()
    } else {
        aliases.join(", ")
    };
    format!(
        "{} ({}; also known as {known_as}; {})",
        name.trim(),
        type_name.trim(),
        summary.trim()
    )
}

fn known_summary(entity: &KnownEntity) -> String {
    one_line(entity.facts.first().map_or("", |fact| fact.value.as_str()))
}

async fn plain_alias(
    decider: &dyn EntityDecider,
    alias: &str,
    subject: &AliasSubject,
    mention: &str,
) -> Option<String> {
    if alias_is_address(alias) && !address_tied_to_name(alias, &subject.name, mention) {
        rewrite::record_alias_rejected(
            &subject.name,
            alias,
            "ip not tied to the name in the mention",
        );
        return None;
    }
    if alias_paired(&subject.name, alias, mention) {
        rewrite::record_alias_paired();
        return Some(alias.to_owned());
    }
    let question = alias_question(alias, subject, mention);
    match ask_alias(decider, &question).await {
        Ok(score) if score.is_finite() && score >= ALIAS_MIN => {
            rewrite::record_alias_accepted();
            Some(alias.to_owned())
        }
        Ok(score) => {
            let shown = if score.is_finite() { score } else { 0.0 };
            rewrite::record_alias_rejected(
                &subject.name,
                alias,
                &format!("p={shown:.2} below {ALIAS_MIN:.1}"),
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, alias, "alias check failed");
            rewrite::record_alias_rejected(&subject.name, alias, "unchecked");
            None
        }
    }
}

async fn ask_alias(decider: &dyn EntityDecider, question: &str) -> Result<f32, WorkerError> {
    match decider.alias_probability(question).await {
        Ok(score) => Ok(score),
        Err(error) => {
            tracing::warn!(%error, "alias check failed; retrying once");
            decider.alias_probability(question).await
        }
    }
}

pub(super) fn new_entity_aliases(
    entity: &str,
    aliases: &[String],
    mention: &str,
    known: &[KnownEntity],
) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for alias in aliases {
        let trimmed = alias.trim();
        let key = canonical_key(trimmed);
        if key.is_empty()
            || key == canonical_key(entity)
            || kept.iter().any(|have| canonical_key(have) == key)
        {
            continue;
        }
        let reason = if alias_claims_known(trimmed, entity, known) {
            Some("another entity's name")
        } else if alias_is_address(trimmed) && !address_tied_to_name(trimmed, entity, mention) {
            Some("ip not tied to the name in the mention")
        } else if !alias_in_evidence(trimmed, mention) {
            Some("not in the mention")
        } else {
            None
        };
        if let Some(reason) = reason {
            rewrite::record_alias_rejected(entity, trimmed, reason);
        } else {
            rewrite::record_alias_accepted();
            kept.push(trimmed.to_owned());
        }
    }
    kept
}

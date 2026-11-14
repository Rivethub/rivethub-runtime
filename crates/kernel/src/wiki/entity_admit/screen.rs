use super::super::entity_match::TypeRoot;
use super::super::entity_match::type_root;
use super::super::entity_match::types_compatible;
use super::super::entity_pass::EntityObservation;
use super::super::entity_pass::KnownEntity;
use super::super::entity_pass::compact_key;
use super::classify::Admit;
use super::classify::admit_name;
use super::classify::canonical_ip;
use super::classify::is_address;
use super::classify::looks_like_sku;

#[must_use]
pub fn screen_observations(
    observations: Vec<EntityObservation>,
    known: &[KnownEntity],
) -> Vec<EntityObservation> {
    let (admitted, rejected) = split_admitted(observations);
    let mut folded = fold_list(admitted);
    for observation in rejected {
        attach_rejected(&mut folded, observation, known);
    }
    fold_list(folded)
}

#[must_use]
pub fn identified_by<'a>(name: &str, known: &'a [KnownEntity]) -> Option<&'a KnownEntity> {
    known
        .iter()
        .filter(|entity| score_entity(name, entity) > 0)
        .max_by_key(|entity| {
            (
                score_entity(name, entity),
                type_root(&entity.type_name) == TypeRoot::Host,
            )
        })
}

fn split_admitted(
    observations: Vec<EntityObservation>,
) -> (Vec<EntityObservation>, Vec<EntityObservation>) {
    let mut admitted = Vec::new();
    let mut rejected = Vec::new();
    for observation in observations {
        if observation.name.trim().is_empty() {
            continue;
        }
        match admit_name(&observation.name) {
            Admit::Keep => admitted.push(observation),
            Admit::Drop(_) => rejected.push(observation),
        }
    }
    (admitted, rejected)
}

fn attach_rejected(
    folded: &mut Vec<EntityObservation>,
    observation: EntityObservation,
    known: &[KnownEntity],
) {
    if let Some(index) = folded
        .iter()
        .position(|kept| points_at(&observation.name, kept))
    {
        if let Some(slot) = folded.get_mut(index) {
            absorb(slot, observation);
        }
        return;
    }
    if let Some(entity) = identified_by(&observation.name, known) {
        folded.push(rewrite_onto(observation, entity));
    }
}

fn fold_list(items: Vec<EntityObservation>) -> Vec<EntityObservation> {
    let mut folded = Vec::new();
    for item in items {
        match folded.iter().position(|kept| same_thing(kept, &item)) {
            Some(index) => merge_at(&mut folded, index, item),
            None => folded.push(item),
        }
    }
    folded
}

fn merge_at(folded: &mut Vec<EntityObservation>, index: usize, item: EntityObservation) {
    let kept_weak = folded
        .get(index)
        .is_some_and(|kept| label_is_weak(&kept.name));
    if kept_weak && !label_is_weak(&item.name) {
        if let Some(weak) = folded.get(index).cloned() {
            folded.swap_remove(index);
            let mut host = item;
            absorb(&mut host, weak);
            folded.push(host);
        }
        return;
    }
    if let Some(slot) = folded.get_mut(index) {
        absorb(slot, item);
    }
}

fn same_thing(left: &EntityObservation, right: &EntityObservation) -> bool {
    if types_compatible(&left.type_name, &right.type_name) && name_overlap(left, right) {
        return true;
    }
    points_at(&right.name, left) || points_at(&left.name, right)
}

fn points_at(name: &str, host: &EntityObservation) -> bool {
    if !label_is_weak(name) {
        return false;
    }
    let facts = fact_lines(host);
    score_against(name, &host.name, &host.aliases, &facts) > 0
}

fn rewrite_onto(mut observation: EntityObservation, entity: &KnownEntity) -> EntityObservation {
    let old = observation.name.clone();
    observation.name.clone_from(&entity.name);
    observation.entity_ref = Some(entity.id);
    if !types_compatible(&observation.type_name, &entity.type_name) {
        observation.type_name.clone_from(&entity.type_name);
    }
    push_alias(&mut observation, &old);
    observation
}

fn absorb(host: &mut EntityObservation, other: EntityObservation) {
    push_alias(host, &other.name);
    for alias in &other.aliases {
        push_alias(host, alias);
    }
    host.facts.extend(other.facts);
    host.history.extend(other.history);
    if host.mention.trim().is_empty() {
        host.mention.clone_from(&other.mention);
    }
    if host.entity_ref.is_none() {
        host.entity_ref = other.entity_ref;
    }
}

fn push_alias(host: &mut EntityObservation, alias: &str) {
    let trimmed = alias.trim();
    if trimmed.is_empty() || compact_key(trimmed) == compact_key(&host.name) {
        return;
    }
    let key = compact_key(trimmed);
    if host.aliases.iter().any(|have| compact_key(have) == key) {
        return;
    }
    host.aliases.push(trimmed.to_owned());
}

fn name_overlap(left: &EntityObservation, right: &EntityObservation) -> bool {
    let right_keys = observation_keys(right);
    observation_keys(left)
        .iter()
        .any(|key| right_keys.iter().any(|other| key == other))
}

fn observation_keys(observation: &EntityObservation) -> Vec<String> {
    let mut keys = vec![compact_key(&observation.name)];
    keys.extend(observation.aliases.iter().map(|alias| compact_key(alias)));
    keys.retain(|key| !key.is_empty());
    keys
}

fn label_is_weak(name: &str) -> bool {
    matches!(admit_name(name), Admit::Drop(_)) || looks_like_sku(name)
}

fn score_entity(name: &str, entity: &KnownEntity) -> u8 {
    let facts: Vec<&str> = entity
        .facts
        .iter()
        .map(|fact| fact.value.as_str())
        .collect();
    score_against(name, &entity.name, &entity.aliases, &facts)
}

fn score_against(name: &str, entity_name: &str, aliases: &[String], facts: &[&str]) -> u8 {
    let key = compact_key(name);
    if key.is_empty() {
        return 0;
    }
    if compact_key(entity_name) == key || aliases.iter().any(|alias| compact_key(alias) == key) {
        return 3;
    }
    let mut score = 0_u8;
    for fact in facts {
        if compact_key(fact) == key {
            return 2;
        }
    }
    if is_address(name) {
        let dotted = canonical_ip(name);
        let digits = compact_key(&dotted);
        let seen = facts.iter().any(|fact| {
            fact.contains(dotted.as_str())
                || (digits.len() >= 6 && compact_key(fact).contains(&digits))
        });
        if seen {
            score = 2;
        }
    }
    if looks_like_sku(name) && key.len() >= 8 {
        let seen = facts.iter().any(|fact| compact_key(fact).contains(&key))
            || aliases.iter().any(|alias| {
                let alias_key = compact_key(alias);
                alias_key.contains(&key) || (key.contains(&alias_key) && alias_key.len() >= 8)
            });
        if seen {
            score = score.max(1);
        }
    }
    score
}

fn fact_lines(observation: &EntityObservation) -> Vec<&str> {
    observation
        .facts
        .iter()
        .map(|fact| fact.value.as_str())
        .collect()
}

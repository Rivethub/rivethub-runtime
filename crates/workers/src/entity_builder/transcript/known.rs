use kernel::wiki::entity_match::ref_verified;
use kernel::wiki::entity_pass::EntityObservation;
use kernel::wiki::entity_pass::KNOWN_LIMIT;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::KnownFact;
use kernel::wiki::entity_pass::ObservedFact;
use kernel::wiki::entity_pass::compact_key;
use kernel::wiki::entity_vocab::canonical_attribute;
use uuid::Uuid;

pub(in crate::entity_builder) fn remember(
    known: &mut Vec<KnownEntity>,
    observations: &[EntityObservation],
) {
    for observation in observations {
        let name = observation.name.trim();
        if compact_key(name).is_empty() {
            continue;
        }
        if let Some(slot) = find_known(known, observation) {
            for fact in &observation.facts {
                upsert_fact(&mut slot.facts, &observation.type_name, fact);
            }
            continue;
        }
        if known.len() >= known_cap() {
            continue;
        }
        known.push(KnownEntity {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            type_name: observation.type_name.trim().to_owned(),
            aliases: folded_aliases(&observation.aliases),
            facts: observation
                .facts
                .iter()
                .filter_map(|fact| known_fact(&observation.type_name, fact))
                .collect(),
        });
    }
}

pub(in crate::entity_builder) fn clear_unknown_refs(
    observations: &mut [EntityObservation],
    known: &[KnownEntity],
) {
    for observation in observations {
        let provisional = observation
            .entity_ref
            .is_some_and(|id| !known.iter().any(|item| item.id == id));
        if provisional {
            observation.entity_ref = None;
        }
    }
}

fn find_known<'a>(
    known: &'a mut [KnownEntity],
    observation: &EntityObservation,
) -> Option<&'a mut KnownEntity> {
    if let Some(id) = observation.entity_ref
        && let Some(index) = known.iter().position(|item| item.id == id)
        && known
            .get(index)
            .is_some_and(|item| ref_verified(&observation.name, &item.name, &item.aliases))
    {
        return known.get_mut(index);
    }
    let name = compact_key(&observation.name);
    let kind = compact_key(&observation.type_name);
    let index = known
        .iter()
        .position(|item| compact_key(&item.name) == name && compact_key(&item.type_name) == kind)?;
    known.get_mut(index)
}

fn upsert_fact(facts: &mut Vec<KnownFact>, type_name: &str, fact: &ObservedFact) {
    let Some(known) = known_fact(type_name, fact) else {
        return;
    };
    let key = compact_key(&known.attribute);
    if let Some(slot) = facts.iter_mut().find(|item| {
        canonical_attribute(type_name, &item.attribute)
            .is_some_and(|attribute| compact_key(&attribute) == key)
            || compact_key(&item.attribute) == key
    }) {
        slot.attribute = known.attribute;
        slot.value = known.value;
        return;
    }
    facts.push(known);
}

fn known_fact(type_name: &str, fact: &ObservedFact) -> Option<KnownFact> {
    let value = fact.value.trim();
    let attribute = canonical_attribute(type_name, &fact.attribute)?;
    if value.is_empty() {
        return None;
    }
    Some(KnownFact {
        attribute,
        value: value.to_owned(),
    })
}

fn folded_aliases(aliases: &[String]) -> Vec<String> {
    let mut folded = Vec::new();
    for alias in aliases {
        push_alias(&mut folded, alias);
    }
    folded
}

fn push_alias(aliases: &mut Vec<String>, alias: &str) {
    let trimmed = alias.trim();
    if compact_key(trimmed).is_empty() {
        return;
    }
    let key = compact_key(trimmed);
    if aliases.iter().any(|have| compact_key(have) == key) {
        return;
    }
    aliases.push(trimmed.to_owned());
}

fn known_cap() -> usize {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(fallback, reason = "PATTERNS FALLBACK: the known-entity cap is 60")
    )]
    let cap = usize::try_from(KNOWN_LIMIT).unwrap_or(60);
    cap
}

use kernel::wiki::entity_pass::{
    EntityLeaf, EntityStoreError, KNOWN_LIMIT, KnownEntity, KnownFact, known_document, known_keys,
    mention_score,
};
use rusqlite::Connection;

use super::{claims, error, identity};

pub(super) fn find(
    conn: &Connection,
    leaves: &[EntityLeaf],
) -> Result<Vec<KnownEntity>, EntityStoreError> {
    let document = known_document(leaves);
    let keys = known_keys(&document);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let body = document.to_lowercase();
    let mut matched = Vec::new();
    for row in identity::entities(conn)? {
        if keys.binary_search(&identity::raw_name(&row.name)).is_err()
            && row.aliases.iter().all(|alias| {
                keys.binary_search(&identity::raw_alias(&alias.key))
                    .is_err()
            })
        {
            continue;
        }
        let aliases: Vec<_> = row.aliases.into_iter().map(|alias| alias.name).collect();
        matched.push((
            mention_score(&body, &row.name, &aliases),
            row.created,
            KnownEntity {
                id: row.id,
                name: row.name,
                type_name: row.type_name.unwrap_or_default(),
                aliases,
                facts: Vec::new(),
            },
        ));
    }
    matched.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.id.cmp(&right.2.id))
    });
    let limit = usize::try_from(KNOWN_LIMIT).map_err(error)?;
    matched
        .into_iter()
        .take(limit)
        .map(|(_, _, mut entity)| {
            entity.facts = claims::current(conn, entity.id)?
                .facts
                .into_iter()
                .map(|fact| KnownFact {
                    attribute: fact.attribute,
                    value: fact.value,
                })
                .collect();
            Ok(entity)
        })
        .collect()
}

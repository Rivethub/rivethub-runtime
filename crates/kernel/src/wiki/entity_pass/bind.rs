use uuid::Uuid;

use super::types::BindHit;
use super::types::BindRequest;
use super::types::SlugOccupant;
use crate::topics::Slug;

#[must_use]
pub fn attribute_key(text: &str) -> Option<String> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: text that is not a slug is no attribute key"
        )
    )]
    let slug = Slug::fold(text).ok();
    slug.map(|slug| slug.as_str().to_owned())
}

#[must_use]
pub fn compact_key(text: &str) -> String {
    attribute_key(text)
        .map(|slug| slug.replace('-', ""))
        .unwrap_or_default()
}

#[must_use]
pub fn choose_slug(
    base: &str,
    type_slug: &str,
    lookup: impl Fn(&str) -> SlugOccupant,
) -> Option<String> {
    let mut candidates = vec![base.to_owned()];
    if !type_slug.is_empty() && type_slug != base {
        candidates.push(format!("{base}-{type_slug}"));
        candidates.push(format!("{type_slug}-{base}"));
    }
    candidates.into_iter().find(|candidate| {
        !candidate.is_empty()
            && matches!(lookup(candidate), SlugOccupant::Free | SlugOccupant::Same)
    })
}

#[must_use]
pub fn pick_existing(request: &BindRequest, hits: &[BindHit]) -> Option<Uuid> {
    if request.ref_exists {
        return request.entity_ref;
    }
    let typed: Vec<&BindHit> = hits
        .iter()
        .filter(|hit| request.type_id == Some(hit.type_id))
        .collect();
    if let Some(hit) = oldest(&typed) {
        return Some(hit.id);
    }
    if request.type_id.is_none() && hits.len() == 1 {
        return hits.first().map(|hit| hit.id);
    }
    None
}

fn oldest<'a>(hits: &[&'a BindHit]) -> Option<&'a BindHit> {
    hits.iter()
        .copied()
        .min_by_key(|hit| (hit.created_at, hit.id))
}

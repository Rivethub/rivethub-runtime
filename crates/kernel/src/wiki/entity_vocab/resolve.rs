use crate::topics::Slug;

use super::catalog::Vocab;
use super::catalog::vocab_for;

#[must_use]
pub fn canonical_attribute(type_name: &str, raw: &str) -> Option<String> {
    let folded = folded(raw)?;
    if reject_bare_other(&folded) {
        return None;
    }
    if let Some(specific) = other_specific(&folded) {
        return Some(format!("other-{specific}"));
    }
    if let Some(key) = vocabulary_key(type_name, raw) {
        return Some(key);
    }
    Some(format!("other-{folded}"))
}

#[must_use]
pub fn vocabulary_key(type_name: &str, raw: &str) -> Option<String> {
    let folded = folded(raw)?;
    let compact = compact(&folded);
    if compact.is_empty() || compact == "other" || folded.starts_with("other-") {
        return None;
    }
    resolve(vocab_for(type_name), &compact)
}

fn resolve(vocab: &Vocab, compact_form: &str) -> Option<String> {
    expansions(compact_form).into_iter().find_map(|candidate| {
        let target = synonym(vocab, &candidate).unwrap_or(candidate);
        spelling(vocab, &target)
    })
}

fn synonym(vocab: &Vocab, compact_form: &str) -> Option<String> {
    vocab
        .synonyms
        .iter()
        .find(|(from, _)| *from == compact_form)
        .map(|(_, target)| (*target).to_owned())
}

fn spelling(vocab: &Vocab, target: &str) -> Option<String> {
    let want = compact_key(target);
    vocab
        .keys
        .iter()
        .find(|key| compact_key(key) == want)
        .map(|key| (*key).to_owned())
}

fn expansions(compact_form: &str) -> Vec<String> {
    let mut forms = vec![compact_form.to_owned()];
    if let Some(stem) = compact_form.strip_suffix("ies")
        && stem.len() >= 3
    {
        forms.push(format!("{stem}y"));
    }
    if let Some(stem) = plural_stem(compact_form) {
        forms.push(stem.to_owned());
    }
    if !compact_form.ends_with('s') {
        forms.push(format!("{compact_form}s"));
    }
    if let Some(stem) = compact_form.strip_suffix('y')
        && !stem.is_empty()
    {
        forms.push(format!("{stem}ies"));
    }
    forms
}

fn plural_stem(compact_form: &str) -> Option<&str> {
    let stem = compact_form.strip_suffix('s')?;
    if compact_form.ends_with("ss") || stem.len() < 3 {
        return None;
    }
    Some(stem)
}

fn reject_bare_other(folded: &str) -> bool {
    folded == "other" || folded == "other-"
}

fn other_specific(folded: &str) -> Option<&str> {
    let specific = folded.strip_prefix("other-")?;
    (!specific.is_empty() && specific != "other").then_some(specific)
}

fn folded(text: &str) -> Option<String> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: text that is not a slug is no vocabulary key"
        )
    )]
    let slug = Slug::fold(text).ok();
    slug.map(|slug| slug.as_str().to_owned())
        .filter(|slug| !slug.is_empty())
}

pub(super) fn compact(text: &str) -> String {
    compact_key(&folded(text).unwrap_or_default())
}

fn compact_key(text: &str) -> String {
    folded(text).unwrap_or_default().replace('-', "")
}

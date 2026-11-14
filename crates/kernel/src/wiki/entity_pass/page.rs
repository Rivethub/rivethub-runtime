use super::types::FactState;
use super::types::LiveFact;

#[must_use]
pub fn prose_article(article: &str) -> String {
    let trimmed = article.trim();
    if let Some((prose, _)) = trimmed.split_once("\n### Current facts\n") {
        return prose.trim().to_owned();
    }
    if trimmed == "### Current facts" || trimmed.starts_with("### Current facts\n") {
        return String::new();
    }
    trimmed.to_owned()
}

#[must_use]
pub fn facts_section(facts: &[LiveFact]) -> String {
    let mut sorted: Vec<&LiveFact> = facts.iter().collect();
    sorted.sort_by(|a, b| a.attribute.cmp(&b.attribute).then(b.as_of.cmp(&a.as_of)));
    let mut out = String::from("### Current facts\n");
    for fact in sorted {
        let ended = if fact.status == FactState::Ended {
            " (ended)"
        } else {
            ""
        };
        out.push_str("\n- **");
        out.push_str(fact.attribute.trim());
        out.push_str("**: ");
        out.push_str(fact.value.trim());
        out.push_str(ended);
        out.push_str(" (as of ");
        out.push_str(&fact.as_of.date_naive().to_string());
        out.push(')');
    }
    out
}

#[must_use]
pub fn article_with_facts(article: &str, facts: &[LiveFact]) -> String {
    let article = article.trim();
    if facts.is_empty() {
        return article.to_owned();
    }
    let section = facts_section(facts);
    if article.is_empty() {
        section
    } else {
        format!("{article}\n\n{section}")
    }
}

#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub fn push_within_budget(budget: &mut usize, line: &str) -> bool {
    let tokens = estimate_tokens(line);
    if tokens > *budget {
        return false;
    }
    *budget -= tokens;
    true
}

#[must_use]
pub fn relative_when(delta: jiff::SignedDuration) -> String {
    let minutes = delta.as_mins().max(0);
    if minutes < 1 {
        "just now".to_owned()
    } else if minutes < 60 {
        format!("{minutes} minutes ago")
    } else if minutes < 60 * 24 {
        format!("{} hours ago", minutes / 60)
    } else if minutes < 60 * 24 * 30 {
        format!("{} days ago", minutes / (60 * 24))
    } else {
        format!("{} months ago", minutes / (60 * 24 * 30))
    }
}

#[must_use]
pub fn char_prefix(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => text.get(..end).unwrap_or(text),
        None => text,
    }
}

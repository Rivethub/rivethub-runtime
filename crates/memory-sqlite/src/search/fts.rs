pub(crate) fn fts_or(text: &str) -> Option<String> {
    fts(text, " OR ")
}

pub(super) fn fts_and(text: &str) -> Option<String> {
    fts(text, " AND ")
}

fn fts(text: &str, join: &str) -> Option<String> {
    let tokens: Vec<String> = text.split_whitespace().filter_map(fts_token).collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(join))
    }
}

fn fts_token(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|ch| !stripped(*ch)).collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(format!("\"{cleaned}\""))
    }
}

const fn stripped(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '^' | '~' | '*' | '(' | ')' | ':')
}

#[cfg(test)]
mod tests {
    use super::fts_or;

    #[test]
    fn recorded_queries_build_or_match_strings() {
        assert_eq!(fts_or("deploy").as_deref(), Some("\"deploy\""));
        assert_eq!(
            fts_or("deploy release").as_deref(),
            Some("\"deploy\" OR \"release\"")
        );
        assert_eq!(
            fts_or("\"unbalanced AND (").as_deref(),
            Some("\"unbalanced\" OR \"AND\"")
        );
        assert_eq!(fts_or("*** ^~ ()::''"), None);
        assert_eq!(
            fts_or("api.example.com:8443").as_deref(),
            Some("\"api.example.com8443\"")
        );
        assert_eq!(fts_or("zzzzqqq").as_deref(), Some("\"zzzzqqq\""));
        assert_eq!(fts_or("staging").as_deref(), Some("\"staging\""));
        assert_eq!(fts_or("rollback").as_deref(), Some("\"rollback\""));
    }
}

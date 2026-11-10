use kernel::memory_core::should_trigram_fallback;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Patience {
    Asked,
    Optional,
}

impl Patience {
    pub(crate) const fn statement_timeout(self) -> &'static str {
        match self {
            Self::Asked => "30s",
            Self::Optional => "3s",
        }
    }
}

fn indexable(token: &str) -> bool {
    let mut run = 0_u8;
    for character in token.chars() {
        run = if character.is_alphanumeric() {
            run + 1
        } else {
            0
        };
        if run == 3 {
            return true;
        }
    }
    false
}

fn bare(token: &str) -> &str {
    let mut token = token;
    loop {
        let trimmed = token
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '?' | '!' | ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']'
                )
            })
            .trim_end_matches(['.', ':']);
        if trimmed == token {
            return token;
        }
        token = trimmed;
    }
}

pub(crate) fn literal_tokens(text: &str, patience: Patience) -> Vec<String> {
    let scans = |token: &str| patience == Patience::Asked || indexable(token);
    let mut tokens: Vec<String> = Vec::new();
    for token in text.split_whitespace().map(bare) {
        if should_trigram_fallback(token)
            && scans(token)
            && !tokens.iter().any(|seen| seen == token)
        {
            tokens.push(token.to_owned());
        }
    }
    let whole = text.trim();
    if tokens.is_empty() && !whole.is_empty() && scans(whole) {
        tokens.push(whole.to_owned());
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::Patience;
    use super::indexable;
    use super::literal_tokens;

    #[test]
    fn a_token_needs_three_letters_or_digits_in_a_row_to_be_scanned() {
        for token in [
            "kern-v2x9",
            "/srv/shared",
            "192.0.2.10",
            "main.rs",
            "read-only?",
        ] {
            assert!(indexable(token), "{token}");
        }
        for token in ["A/B", "v2", "4k", "a.b", "x:y", ""] {
            assert!(!indexable(token), "{token}");
        }
    }

    #[test]
    fn an_optional_scan_leaves_out_what_the_index_cannot_narrow() {
        let text = "the kern-v2x9 kernel order A/B work under /srv/shared";
        assert_eq!(
            literal_tokens(text, Patience::Optional),
            ["kern-v2x9", "/srv/shared"]
        );
        assert_eq!(
            literal_tokens("A/B", Patience::Optional),
            Vec::<String>::new()
        );
        assert_eq!(
            literal_tokens("plain words", Patience::Optional),
            ["plain words"]
        );
    }

    #[test]
    fn a_scan_asked_for_by_name_keeps_every_token() {
        let text = "the kern-v2x9 kernel order A/B work under /srv/shared";
        assert_eq!(
            literal_tokens(text, Patience::Asked),
            ["kern-v2x9", "A/B", "/srv/shared"]
        );
        assert_eq!(literal_tokens("A/B", Patience::Asked), ["A/B"]);
        assert_eq!(
            literal_tokens("is it in deny.toml? or (main.rs).", Patience::Asked),
            ["deny.toml", "main.rs"]
        );
    }
}

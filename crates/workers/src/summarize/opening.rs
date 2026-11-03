const WORK: &str = "The work";

pub(super) fn work_first(text: &str, agent: &str) -> String {
    let text = text.trim_start();
    let words: Vec<String> = text
        .split_whitespace()
        .take(4)
        .map(str::to_ascii_lowercase)
        .collect();
    let word = |index: usize| words.get(index).map_or("", String::as_str);
    if matches!(word(0), "in" | "during")
        && matches!(word(1), "this" | "the")
        && word(2).trim_end_matches(',') == "batch"
    {
        return capitalized(skip_words(text, 3));
    }
    if !(matches!(word(0), "the" | "this") && word(1) == "batch") {
        return text.to_owned();
    }
    let rest = if word(2) == "of" && word(3) == "work" {
        skip_words(text, 4)
    } else {
        skip_words(text, 2)
    };
    let mut next = rest.split_whitespace();
    let describes = next.next().is_some_and(|verb| matches!(verb, "was" | "is"))
        && next
            .next()
            .is_none_or(|following| !following.ends_with("ing"));
    let subject = if describes || agent.trim().is_empty() {
        WORK
    } else {
        agent.trim()
    };
    format!("{subject} {rest}")
}

fn skip_words(text: &str, count: usize) -> &str {
    let mut rest = text.trim_start();
    for _ in 0..count {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = rest.get(end..).unwrap_or_default().trim_start();
    }
    rest
}

fn capitalized(text: &str) -> String {
    let mut letters = text.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::work_first;

    #[test]
    fn a_summary_opens_with_the_work_not_the_batch() {
        let cases = [
            (
                "The batch began by reading the vLLM provider.",
                "rivet-claude began by reading the vLLM provider.",
            ),
            (
                "This batch of work checked the local tool versions.",
                "rivet-claude checked the local tool versions.",
            ),
            (
                "The batch was working through the core task.",
                "rivet-claude was working through the core task.",
            ),
            (
                "The batch was preparatory investigation for the fix.",
                "The work was preparatory investigation for the fix.",
            ),
            (
                "In this batch, rivet-grok fixed the lint.",
                "Rivet-grok fixed the lint.",
            ),
            (
                "Rivet-claude reviewed PR #1142.",
                "Rivet-claude reviewed PR #1142.",
            ),
        ];
        for (summary, expected) in cases {
            assert_eq!(work_first(summary, "rivet-claude"), expected);
        }
    }
}

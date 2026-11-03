const SNIPPET_CHARS: usize = 480;
const LEAD_CHARS: usize = 120;

pub(crate) fn snippet(content: &str, query: &str) -> String {
    let lower = content.to_lowercase();
    let found = query
        .split_whitespace()
        .filter(|word| word.chars().count() >= 3)
        .filter_map(|word| lower.find(&word.to_lowercase()))
        .min()
        .filter(|at| lower.len() == content.len() && content.is_char_boundary(*at));
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    if chars.len() <= SNIPPET_CHARS {
        return content.to_owned();
    }
    let hit = found.map_or(0, |at| chars.partition_point(|(index, _)| *index < at));
    let start = hit
        .saturating_sub(LEAD_CHARS)
        .min(chars.len() - SNIPPET_CHARS);
    let end = start + SNIPPET_CHARS;
    let from = chars.get(start).map_or(0, |(index, _)| *index);
    let to = chars.get(end).map_or(content.len(), |(index, _)| *index);
    let mut text = String::with_capacity(to - from + 6);
    if start > 0 {
        text.push('…');
    }
    text.push_str(content.get(from..to).unwrap_or_default());
    if end < chars.len() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::snippet;

    #[test]
    fn a_snippet_shows_the_passage_around_the_first_match() {
        assert_eq!(snippet("short text", "gpubox"), "short text");
        let long = format!("{}gpubox answered{}", "é ".repeat(400), " tail".repeat(200));
        let shown = snippet(&long, "GPUBOX vllm");
        assert!(shown.starts_with('…') && shown.ends_with('…'));
        assert!(shown.contains("gpubox answered"));
        assert_eq!(shown.chars().count(), 482);
        let plain = snippet(&"word ".repeat(200), "nothing");
        assert!(plain.starts_with("word") && plain.chars().count() == 481);
    }
}

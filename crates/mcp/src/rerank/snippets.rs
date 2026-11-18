use std::collections::HashSet;

use tokio::time::Instant;

pub(super) fn extract(
    query: &str,
    title: Option<&str>,
    rendered: &str,
    width: usize,
    deadline: Instant,
) -> Result<String, &'static str> {
    let window = best_window(rendered, query, width, deadline)?;
    match lead_line(title, rendered) {
        Some(lead) if !fronted(&window, lead) => {
            let lead: String = lead.chars().take(width).collect();
            Ok(format!("{lead}\n{window}"))
        }
        Some(_) | None => Ok(window),
    }
}

fn lead_line<'a>(title: Option<&'a str>, rendered: &'a str) -> Option<&'a str> {
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        return title
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty());
    }
    let line = rendered.lines().next()?.trim();
    (line.starts_with('#') || line.starts_with("**") || line.starts_with("- [")).then_some(line)
}

fn fronted(window: &str, lead: &str) -> bool {
    window
        .strip_prefix(lead)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\n'))
}

fn best_window(
    text: &str,
    query: &str,
    width: usize,
    deadline: Instant,
) -> Result<String, &'static str> {
    check_deadline(deadline)?;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return Ok(text.to_owned());
    }
    let folded: Vec<_> = chars.iter().map(char::to_ascii_lowercase).collect();
    let words = query_words(query);
    let spans = words
        .into_iter()
        .filter(|word| word.len() <= width)
        .map(|word| find_spans(&folded, &word, deadline).map(|positions| (word.len(), positions)))
        .collect::<Result<Vec<_>, _>>()?;
    let start = best_start(chars.len() - width, width, &spans, deadline)?;
    chars
        .get(start..start + width)
        .map(|window| window.iter().collect())
        .ok_or("invalid snippet window")
}

fn query_words(query: &str) -> HashSet<Vec<char>> {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.chars().map(|ch| ch.to_ascii_lowercase()).collect())
        .collect()
}

fn find_spans(text: &[char], word: &[char], deadline: Instant) -> Result<Vec<usize>, &'static str> {
    let mut positions = Vec::new();
    for (start, chars) in text.windows(word.len()).enumerate() {
        if start % 1024 == 0 {
            check_deadline(deadline)?;
        }
        if chars == word
            && boundary(text, start.checked_sub(1))
            && boundary(text, Some(start + word.len()))
        {
            positions.push(start);
        }
    }
    Ok(positions)
}

fn boundary(text: &[char], at: Option<usize>) -> bool {
    at.and_then(|index| text.get(index))
        .is_none_or(|ch| !ch.is_ascii_alphanumeric())
}

fn best_start(
    last: usize,
    width: usize,
    spans: &[(usize, Vec<usize>)],
    deadline: Instant,
) -> Result<usize, &'static str> {
    let mut best = 0;
    let mut most = 0;
    for start in 0..=last {
        check_deadline(deadline)?;
        let score = spans
            .iter()
            .filter(|(length, positions)| {
                let at = positions.partition_point(|pos| *pos < start);
                positions
                    .get(at)
                    .is_some_and(|pos| pos + length <= start + width)
            })
            .count();
        if score > most {
            most = score;
            best = start;
        }
        if most == spans.len() {
            break;
        }
    }
    Ok(best)
}

fn check_deadline(deadline: Instant) -> Result<(), &'static str> {
    if Instant::now() >= deadline {
        Err("timeout")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Instant, extract};
    use std::time::Duration;

    #[test]
    fn selects_the_earliest_richest_window_of_whole_ascii_words() -> Result<(), &'static str> {
        let text = "chalkboard éééé chalk éééé glaze";
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            extract("Chalk chalk glaze", None, text, 10, deadline)?,
            "éééé chalk"
        );
        assert_eq!(
            extract("chalk glaze", None, text, 16, deadline)?,
            "chalk éééé glaze"
        );
        Ok(())
    }

    #[test]
    fn adds_the_title_or_rendered_heading_when_outside_the_window() -> Result<(), &'static str> {
        let text = "- [hit]\néééééééé kiln";
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            extract("kiln", None, text, 8, deadline)?,
            "- [hit]\nééé kiln"
        );
        let title = Some("  Vessel\nignored line");
        assert_eq!(extract("kiln", title, text, 6, deadline)?, "Vessel\né kiln");
        assert_eq!(extract("kiln", None, text, 300, deadline)?, text);
        let inline = "- [hit] éééééééé kiln";
        assert_eq!(
            extract("kiln", None, inline, 6, deadline)?,
            "- [hit\né kiln"
        );
        Ok(())
    }
}

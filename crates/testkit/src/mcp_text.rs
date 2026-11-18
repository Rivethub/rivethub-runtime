use regex::Regex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TextMismatch {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
    #[error("text differs at {position}: expected {expected:?}, got {actual:?}")]
    Literal {
        position: usize,
        expected: String,
        actual: String,
    },
    #[error("invalid variable field at {position}: {actual:?}")]
    Variable { position: usize, actual: String },
    #[error(transparent)]
    Number(#[from] std::num::ParseFloatError),
    #[error(transparent)]
    Date(#[from] jiff::Error),
}

pub fn compare_memory_text(
    actual: &str,
    expected: &str,
    variable_scores: bool,
) -> Result<(), TextMismatch> {
    let fields = Regex::new(if expected.starts_with("No messages found.\n_window=") {
        r"since=\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z"
    } else if variable_scores {
        r"\b\d+[mhd] ago\b|score: -?\d+\.\d{3}"
    } else {
        r"\b\d+[mhd] ago\b"
    })?;
    let mut expected_position = 0;
    let mut actual_position = 0;
    for field in fields.find_iter(expected) {
        let literal = &expected[expected_position..field.start()];
        match_literal(actual, &mut actual_position, literal)?;
        let remaining = &actual[actual_position..];
        let value = fields
            .find(remaining)
            .filter(|value| value.start() == 0)
            .ok_or_else(|| TextMismatch::Variable {
                position: actual_position,
                actual: remaining.to_owned(),
            })?;
        let expected_score = field.as_str().starts_with("score:");
        if expected_score != value.as_str().starts_with("score:") {
            return Err(TextMismatch::Variable {
                position: actual_position,
                actual: value.as_str().to_owned(),
            });
        }
        if expected_score {
            let score = value
                .as_str()
                .trim_start_matches("score: ")
                .parse::<f64>()?;
            if !score.is_finite() {
                return Err(TextMismatch::Variable {
                    position: actual_position,
                    actual: value.as_str().to_owned(),
                });
            }
        }
        if let Some(timestamp) = value.as_str().strip_prefix("since=") {
            wire::time::parse_rfc3339(timestamp)?;
        }
        actual_position += value.end();
        expected_position = field.end();
    }
    match_literal(actual, &mut actual_position, &expected[expected_position..])?;
    if actual_position != actual.len() {
        return Err(TextMismatch::Literal {
            position: actual_position,
            expected: String::new(),
            actual: actual[actual_position..].to_owned(),
        });
    }
    Ok(())
}

fn match_literal(actual: &str, position: &mut usize, expected: &str) -> Result<(), TextMismatch> {
    if !actual[*position..].starts_with(expected) {
        return Err(TextMismatch::Literal {
            position: *position,
            expected: expected.to_owned(),
            actual: actual[*position..].to_owned(),
        });
    }
    *position += expected.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compare_memory_text;

    #[test]
    fn only_declared_variable_fields_may_differ() {
        assert!(
            compare_memory_text(
                "hit 44d ago score: 0.123 tags: project:Kiwi",
                "hit 43d ago score: 0.123 tags: project:Kiwi",
                false
            )
            .is_ok()
        );
        assert!(
            compare_memory_text(
                "hit 44d ago score: 0.999 tags: project:Kiwi",
                "hit 43d ago score: 0.123 tags: project:Kiwi",
                false
            )
            .is_err()
        );
        assert!(
            compare_memory_text(
                "hit 44d ago score: 0.999 tags: project:Kiwi",
                "hit 43d ago score: 0.123 tags: project:Kiwi",
                true
            )
            .is_ok()
        );
        assert!(
            compare_memory_text(
                "hit 44d ago score: 0.123 tags: project:Other",
                "hit 43d ago score: 0.123 tags: project:Kiwi",
                false
            )
            .is_err()
        );
        assert!(compare_memory_text("hit yesterday", "hit 43d ago", false).is_err());
        assert!(compare_memory_text("hit 1d ago extra", "hit 43d ago", false).is_err());
    }

    #[test]
    fn relative_window_dates_must_be_valid_and_preserve_the_rest_of_the_text() {
        let expected = "No messages found.\n_window=\"today\" since=2026-10-16T00:00:00.000Z_";
        assert!(
            compare_memory_text(
                "No messages found.\n_window=\"today\" since=2026-10-17T00:00:00.000Z_",
                expected,
                false
            )
            .is_ok()
        );
        assert!(
            compare_memory_text(
                "No messages found.\n_window=\"today\" since=2026-99-17T00:00:00.000Z_",
                expected,
                false
            )
            .is_err()
        );
        assert!(
            compare_memory_text(
                "No messages found.\n_window=\"yesterday\" since=2026-10-17T00:00:00.000Z_",
                expected,
                false
            )
            .is_err()
        );
    }
}

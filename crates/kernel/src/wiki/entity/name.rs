use crate::cleanup::EntityType;
use crate::filing::MAX_NAME_CHARS;
use crate::filing::MAX_NAME_WORDS;
use crate::filing::file_name;
use crate::filing::identifier;

const TEXT_MARKS: [char; 12] = ['(', ')', '[', ']', '{', '}', '`', '"', '=', '<', '>', '|'];
const SENTENCE_ENDS: [char; 5] = ['.', '?', '!', ':', ','];
const PACKAGE_SCOPE: char = '@';
const LABEL_CHARS: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
enum Connective {
    And,
    Or,
    On,
    In,
    For,
    Of,
    With,
    Via,
    Vs,
    From,
    To,
    At,
    By,
    Into,
}

#[must_use]
pub(super) fn entity_name(title: &str, kind: Option<EntityType>) -> Option<&str> {
    let name = unqualified(title);
    let words: Vec<&str> = name.split_whitespace().collect();
    let short = !words.is_empty()
        && words.len() <= MAX_NAME_WORDS
        && name.chars().count() <= MAX_NAME_CHARS;
    let prose = name.contains(TEXT_MARKS)
        || name.contains("::")
        || name.contains(": ")
        || name.ends_with(SENTENCE_ENDS);
    let described = words.len() > 1 && words.iter().any(|word| word.parse::<Connective>().is_ok());
    let code = words
        .iter()
        .any(|word| source_file(word) || identifier(word) || code_symbol(word) || path(word, kind));
    let shaped = match kind {
        Some(EntityType::Hosts) => hostname(name),
        Some(EntityType::Domains) => hostname(name) && domain(name),
        Some(EntityType::Repos) => repository(name),
        Some(_) | None => true,
    };
    (short && !prose && !described && !code && shaped).then_some(name)
}

fn unqualified(title: &str) -> &str {
    let title = title.trim();
    match title.rfind('(') {
        Some(open) if open > 0 && title.ends_with(')') => {
            title.get(..open).map_or(title, str::trim_end)
        }
        Some(_) | None => title,
    }
}

fn source_file(word: &str) -> bool {
    file_name(word) && word.starts_with(|ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn code_symbol(word: &str) -> bool {
    let lead = word.chars().take_while(char::is_ascii_lowercase).count();
    let mut rest = word.chars().skip(lead);
    lead >= 2
        && rest.next().is_some_and(|ch| ch.is_ascii_uppercase())
        && rest.next().is_some_and(|ch| ch.is_ascii_lowercase())
}

fn path(word: &str, kind: Option<EntityType>) -> bool {
    word.contains('\\')
        || (word.contains('/')
            && !word.starts_with(PACKAGE_SCOPE)
            && kind != Some(EntityType::Repos))
}

fn hostname(name: &str) -> bool {
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= LABEL_CHARS
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    })
}

fn domain(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, top)| {
        top.chars().count() >= 2 && top.chars().all(|ch| ch.is_ascii_alphabetic())
    })
}

fn repository(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    parts.len() <= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        })
}

#[cfg(test)]
mod tests {
    use super::EntityType;
    use super::entity_name;

    #[test]
    fn a_proper_name_is_kept_and_a_trailing_qualifier_is_dropped() {
        for (title, kind, expected) in [
            ("buildbox", Some(EntityType::Hosts), "buildbox"),
            (
                "SYS-4029GP-TVRT",
                Some(EntityType::Hosts),
                "SYS-4029GP-TVRT",
            ),
            (
                "box7 (container running the gateway on port 5174)",
                Some(EntityType::Hosts),
                "box7",
            ),
            (
                "Qwen3.8-Flash-Next",
                Some(EntityType::Models),
                "Qwen3.8-Flash-Next",
            ),
            (
                "acmekit.example",
                Some(EntityType::Domains),
                "acmekit.example",
            ),
            ("acme/acmekit", Some(EntityType::Repos), "acme/acmekit"),
            ("@acme/kit-core", None, "@acme/kit-core"),
            (
                "1Password MCP server",
                Some(EntityType::Services),
                "1Password MCP server",
            ),
            ("Node.js", None, "Node.js"),
            ("acme_memory", Some(EntityType::Databases), "acme_memory"),
        ] {
            assert_eq!(entity_name(title, kind), Some(expected), "{title}");
        }
    }

    #[test]
    fn activities_descriptions_files_and_code_are_not_names() {
        for (title, kind) in [
            ("Building AcmeKit Android on box7", Some(EntityType::Hosts)),
            (
                "buildbox AcmeKit pairing.js update",
                Some(EntityType::Hosts),
            ),
            ("AcmeKit build box", Some(EntityType::Hosts)),
            ("Pixel 10 Pro", Some(EntityType::Hosts)),
            ("Acme Cloud", Some(EntityType::Domains)),
            ("AcmeKit repository", Some(EntityType::Repos)),
            ("Running Vitest in an AcmeKit worktree", None),
            ("Jetpack Compose: @Composable calls in inline let/if", None),
            ("protocol::tool_result::ToolResult", None),
            ("applyBackfillTimestamps", None),
            ("kit-tools.test.ts", None),
            ("apps/acmekit-electron", None),
            ("build 3f9a2c1d", None),
            ("Verifying the CLI support for `read --format ansi`", None),
        ] {
            assert_eq!(entity_name(title, kind), None, "{title}");
        }
    }
}

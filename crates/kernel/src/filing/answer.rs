use crate::topics::TopicId;
use crate::topics::TopicName;
use crate::topics::fold;

pub(crate) const MAX_NAME_WORDS: usize = 4;
pub(crate) const MAX_NAME_CHARS: usize = 40;
const MIN_ID_CHARS: usize = 6;
const MAX_VERSION_DIGITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    File(TopicId),
    Suggest {
        name: TopicName,
        under: Option<TopicId>,
    },
    Nothing,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, strum::Display, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Refusal {
    Deleted,
    Title,
    Path,
    Ticket,
    Version,
    Branch,
    Identifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
enum BranchWord {
    Feat,
    Fix,
    Chore,
    Refactor,
    Docs,
    Hotfix,
    Release,
}

impl Refusal {
    #[must_use]
    pub fn of(name: &TopicName, title: Option<&str>) -> Option<Self> {
        let text = name.as_str();
        let words: Vec<&str> = text.split_whitespace().collect();
        if title.is_some_and(|title| fold(title) == fold(text))
            || words.len() > MAX_NAME_WORDS
            || text.chars().count() > MAX_NAME_CHARS
            || text.ends_with(['.', '?', '!', ':'])
        {
            return Some(Self::Title);
        }
        if text.contains(['/', '\\', '~'])
            || text.starts_with('.')
            || words.iter().any(|word| file_name(word))
        {
            return Some(Self::Path);
        }
        if text.contains('#') || words.iter().any(|word| ticket(word)) || pr_number(&words) {
            return Some(Self::Ticket);
        }
        if words.iter().any(|word| version(word)) {
            return Some(Self::Version);
        }
        if branch(&words) {
            return Some(Self::Branch);
        }
        if words.iter().any(|word| identifier(word)) {
            return Some(Self::Identifier);
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum FileExtension {
    Md,
    Rs,
    Ts,
    Tsx,
    Js,
    Jsx,
    Mjs,
    Cjs,
    Py,
    Json,
    Jsonl,
    Yaml,
    Yml,
    Toml,
    Sh,
    Txt,
    Log,
    Sql,
    Html,
    Css,
    Go,
    Kt,
    Java,
    Swift,
    Lock,
    Env,
    Conf,
    Cfg,
    Ini,
    Xml,
    Csv,
    Png,
    Jpg,
    Svg,
    Pdf,
}

pub(crate) fn file_name(word: &str) -> bool {
    word.trim_matches(|ch: char| !ch.is_alphanumeric())
        .rsplit_once('.')
        .is_some_and(|(left, right)| !left.is_empty() && right.parse::<FileExtension>().is_ok())
}

fn ticket(word: &str) -> bool {
    word.split_once('-').is_some_and(|(left, right)| {
        !left.is_empty()
            && left.chars().all(char::is_alphabetic)
            && !right.is_empty()
            && right.chars().all(|ch| ch.is_ascii_digit())
    })
}

fn pr_number(words: &[&str]) -> bool {
    words.windows(2).any(|pair| {
        pair.first()
            .is_some_and(|word| word.eq_ignore_ascii_case("pr"))
            && pair
                .get(1)
                .is_some_and(|word| word.chars().all(|ch| ch.is_ascii_digit()))
    })
}

fn version(word: &str) -> bool {
    let lower = word
        .trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase();
    let short = lower.strip_prefix('v').is_some_and(|rest| {
        !rest.is_empty()
            && rest.chars().count() <= MAX_VERSION_DIGITS
            && rest.chars().all(|ch| ch.is_ascii_digit())
    });
    short
        || lower.rsplit_once('.').is_some_and(|(left, right)| {
            left.ends_with(|ch: char| ch.is_ascii_digit())
                && !right.is_empty()
                && right.chars().all(|ch| ch.is_ascii_digit())
        })
}

fn branch(words: &[&str]) -> bool {
    words.first().is_some_and(|first| {
        first
            .split_once('-')
            .is_some_and(|(head, _)| head.parse::<BranchWord>().is_ok())
    })
}

pub(crate) fn identifier(word: &str) -> bool {
    word.chars().count() >= MIN_ID_CHARS
        && word.chars().all(|ch| ch.is_ascii_hexdigit())
        && word.chars().any(|ch| ch.is_ascii_digit())
}

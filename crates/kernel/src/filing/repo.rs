use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::topics::Redirect;
use crate::topics::TopicId;
use crate::topics::TopicKind;
use crate::topics::TopicListing;
use crate::topics::fold;

#[expect(clippy::expect_used)]
static REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)github\.com/[\w.-]+/([\w.-]+)|(?:^|[\s(`'\x22])[~.]?/((?:[\w.-]+/)*[\w.-]+)|\b([\w.-]+)\s*(?:#\d+|\bPRs?\s*#?\d+|\bpull request)|\b(?:PR|pull request)\s*#?\d+\s+(?:in|on|for|of|to)\s+(?:the\s+)?([\w.-]+)",
    )
    .expect("the repo reference pattern is a constant")
});

#[must_use]
pub fn repo_topic(
    topics: &[TopicListing],
    redirects: &[Redirect],
    texts: &[&str],
) -> Option<TopicId> {
    let references = references(texts);
    let owners: BTreeSet<TopicId> = topics
        .iter()
        .map(|listing| &listing.topic)
        .filter(|topic| topic.kind == TopicKind::Work && topic.parent.is_none())
        .filter(|topic| {
            std::iter::once(topic.slug.as_str().to_owned())
                .chain(topic.aliases.iter().map(|alias| fold(alias.as_str())))
                .chain(
                    redirects
                        .iter()
                        .filter(|redirect| redirect.topic == topic.id)
                        .map(|redirect| redirect.slug.as_str().to_owned()),
                )
                .any(|key| !key.is_empty() && references.contains(&key))
        })
        .map(|topic| topic.id)
        .collect();
    let mut owners = owners.into_iter();
    match (owners.next(), owners.next()) {
        (Some(owner), None) => Some(owner),
        (Some(_), Some(_)) | (None, _) => None,
    }
}

#[must_use]
pub fn references(texts: &[&str]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for text in texts {
        for captures in REPO.captures_iter(text) {
            for group in captures.iter().skip(1).flatten() {
                found.extend(
                    group
                        .as_str()
                        .split('/')
                        .map(fold)
                        .filter(|segment| !segment.is_empty()),
                );
            }
        }
    }
    found
}

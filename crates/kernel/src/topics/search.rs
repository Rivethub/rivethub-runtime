use std::cmp::Ordering;

use super::Redirect;
use super::Slug;
use super::TopicError;
use super::TopicId;
use super::TopicKind;
use super::TopicListing;
use super::values::fold;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicKey {
    Id(TopicId),
    Slug(TopicKind, Slug),
}

impl TopicKey {
    pub fn parse(text: &str) -> Result<Self, TopicError> {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let key = match text.trim().parse::<TopicId>() {
            Ok(id) => Self::Id(id),
            Err(_) => Self::Slug(TopicKind::Work, Slug::fold(text)?),
        };
        Ok(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicQuery {
    prefix: String,
    folded: String,
}

impl TopicQuery {
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self {
            prefix: text.trim().to_lowercase(),
            folded: fold(text),
        }
    }

    fn matches(&self, listing: &TopicListing, redirects: &[Redirect]) -> bool {
        if self.prefix.is_empty() {
            return true;
        }
        let topic = &listing.topic;
        topic.name.as_str().to_lowercase().starts_with(&self.prefix)
            || topic
                .aliases
                .iter()
                .any(|alias| alias.key().starts_with(&self.prefix))
            || (!self.folded.is_empty()
                && (self.folded_hit(topic.slug.as_str())
                    || topic
                        .aliases
                        .iter()
                        .any(|alias| self.folded_hit(&fold(alias.as_str())))
                    || redirects
                        .iter()
                        .filter(|redirect| redirect.topic == topic.id)
                        .any(|redirect| self.folded_hit(redirect.slug.as_str()))))
    }

    fn folded_hit(&self, slug: &str) -> bool {
        slug.starts_with(&self.folded)
            || slug
                .match_indices('-')
                .any(|(at, _)| slug[at + 1..].starts_with(&self.folded))
    }
}

#[must_use]
pub fn by_activity(left: &TopicListing, right: &TopicListing) -> Ordering {
    right
        .last_active
        .cmp(&left.last_active)
        .then_with(|| left.topic.slug.cmp(&right.topic.slug))
}

#[must_use]
pub fn rank(
    listings: Vec<TopicListing>,
    redirects: &[Redirect],
    query: &TopicQuery,
    limit: usize,
) -> Vec<TopicListing> {
    let mut hits: Vec<TopicListing> = listings
        .into_iter()
        .filter(|listing| query.matches(listing, redirects))
        .collect();
    hits.sort_by(by_activity);
    hits.truncate(limit);
    hits
}

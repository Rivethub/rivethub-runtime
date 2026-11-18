use jiff::Timestamp;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::builder::WikiStoreError;
use super::page::FoldMark;
use super::page::topic_identity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsolidateWiki {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_limit_clusters")]
    pub limit_clusters: u32,
    #[serde(default = "default_min_cluster_size")]
    pub min_cluster_size: u32,
}

const fn default_limit_clusters() -> u32 {
    50
}

const fn default_min_cluster_size() -> u32 {
    2
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiTopicStats {
    pub slug: String,
    pub title: String,
    pub citations: u64,
    pub history: u64,
    pub created_at: Timestamp,
    pub bound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiFold {
    pub cluster: String,
    pub parent: String,
    pub children: Vec<String>,
    pub mark: FoldMark,
}

#[async_trait]
pub trait WikiConsolidateStore: Send + Sync {
    async fn wiki_topic_stats(&self) -> Result<Vec<WikiTopicStats>, WikiStoreError>;

    async fn fold_wiki(
        &self,
        fold: &WikiFold,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError>;
}

#[must_use]
pub fn consolidation_plan(topics: &[WikiTopicStats], options: &ConsolidateWiki) -> Vec<WikiFold> {
    let topics: Vec<&WikiTopicStats> = topics
        .iter()
        .filter(|topic| is_normalized(&topic.slug))
        .collect();
    let identity = identity_folds(&topics);
    let folded: BTreeSet<&str> = identity
        .iter()
        .flat_map(|(_, fold)| fold.children.iter().map(String::as_str))
        .collect();
    let remaining: Vec<String> = topics
        .iter()
        .filter(|topic| !folded.contains(topic.slug.as_str()))
        .map(|topic| topic.slug.clone())
        .collect();
    let bound: BTreeSet<&str> = topics
        .iter()
        .filter(|topic| topic.bound)
        .map(|topic| topic.slug.as_str())
        .collect();
    let mut planned: Vec<(usize, WikiFold)> = identity
        .into_iter()
        .chain(
            stem_clusters(&remaining)
                .into_iter()
                .filter_map(|(cluster, members)| {
                    let size = members.len();
                    fold_for(cluster, &members, &bound).map(|fold| (size, fold))
                }),
        )
        .filter(|(size, _)| !u32::try_from(*size).is_ok_and(|size| size < options.min_cluster_size))
        .collect();
    planned.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cluster.cmp(&right.1.cluster))
    });
    planned
        .into_iter()
        .zip(0..options.limit_clusters)
        .map(|((_, fold), _)| fold)
        .collect()
}

fn identity_folds(topics: &[&WikiTopicStats]) -> Vec<(usize, WikiFold)> {
    let mut groups: BTreeMap<String, Vec<(usize, &WikiTopicStats)>> = BTreeMap::new();
    for topic in topics {
        let identity = topic_identity(&topic.slug, &topic.title);
        groups
            .entry(identity.key)
            .or_default()
            .push((identity.noise, *topic));
    }
    groups
        .into_iter()
        .filter_map(|(cluster, members)| {
            let parent = survivor(&members)?;
            let mut children: Vec<String> = members
                .iter()
                .filter(|(_, topic)| topic.slug != parent && !topic.bound)
                .map(|(_, topic)| topic.slug.clone())
                .collect();
            children.sort();
            (!children.is_empty()).then_some((
                members.len(),
                WikiFold {
                    cluster,
                    parent,
                    children,
                    mark: FoldMark::Merged,
                },
            ))
        })
        .collect()
}

fn survivor(members: &[(usize, &WikiTopicStats)]) -> Option<String> {
    members
        .iter()
        .min_by_key(|&&(noise, topic)| {
            (
                !topic.bound,
                noise,
                Reverse(topic.citations),
                Reverse(topic.history),
                topic.created_at,
                topic.slug.len(),
                topic.slug.as_str(),
            )
        })
        .map(|(_, topic)| topic.slug.clone())
}

fn fold_for(cluster: String, members: &[String], bound: &BTreeSet<&str>) -> Option<WikiFold> {
    let parent = canonical_parent(&cluster, members)?;
    let children: Vec<String> = members
        .iter()
        .filter(|slug| {
            **slug != parent && !bound.contains(slug.as_str()) && is_slug_variant(slug, &parent)
        })
        .cloned()
        .collect();
    (!children.is_empty()).then_some(WikiFold {
        cluster,
        parent,
        children,
        mark: FoldMark::Merged,
    })
}

fn canonical_parent(cluster: &str, members: &[String]) -> Option<String> {
    if members.iter().any(|slug| slug == cluster) {
        return Some(cluster.to_owned());
    }
    members
        .iter()
        .min_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)))
        .cloned()
}

fn stem_clusters(slugs: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for slug in slugs.iter().filter(|slug| is_normalized(slug)) {
        let parts: Vec<&str> = slug.split('-').collect();
        let key = match parts.as_slice() {
            [first, second, _, ..] => format!("{first}-{second}"),
            _ => slug.clone(),
        };
        groups.entry(key).or_default().push(slug.clone());
    }
    groups
}

#[must_use]
pub fn is_slug_variant(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() || left == right {
        return left == right && !left.is_empty();
    }
    hyphen_child(left, right) || hyphen_child(right, left)
}

fn hyphen_child(child: &str, parent: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('-'))
}

fn is_normalized(slug: &str) -> bool {
    !slug.is_empty()
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

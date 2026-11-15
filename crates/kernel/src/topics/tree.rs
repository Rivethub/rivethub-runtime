use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::Topic;
use super::TopicError;
use super::TopicId;
use super::TopicKey;
use super::TopicKind;
use super::TopicStore;

pub const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopicTree {
    parents: BTreeMap<TopicId, Option<TopicId>>,
    entities: BTreeSet<TopicId>,
}

impl TopicTree {
    pub fn new(edges: impl IntoIterator<Item = (TopicId, Option<TopicId>, TopicKind)>) -> Self {
        let mut tree = Self::default();
        for (id, parent, kind) in edges {
            tree.parents.insert(id, parent);
            if kind == TopicKind::Entity {
                tree.entities.insert(id);
            }
        }
        tree
    }

    pub fn of<'a>(topics: impl IntoIterator<Item = &'a Topic>) -> Self {
        Self::new(
            topics
                .into_iter()
                .map(|topic| (topic.id, topic.parent, topic.kind)),
        )
    }

    pub fn ancestors(&self, id: TopicId) -> impl Iterator<Item = TopicId> + '_ {
        std::iter::successors(self.parent(id), |above| self.parent(*above)).take(self.parents.len())
    }

    #[must_use]
    pub fn depth(&self, id: TopicId) -> usize {
        self.ancestors(id).count() + 1
    }

    #[must_use]
    pub fn within(&self, id: TopicId, ancestor: TopicId) -> bool {
        id == ancestor || self.ancestors(id).any(|above| above == ancestor)
    }

    #[must_use]
    pub fn children(&self, id: TopicId) -> Vec<TopicId> {
        self.parents
            .iter()
            .filter(|(_, parent)| **parent == Some(id))
            .map(|(child, _)| *child)
            .collect()
    }

    #[must_use]
    pub fn height(&self, id: TopicId) -> usize {
        self.height_within(id, self.parents.len())
    }

    #[must_use]
    pub fn moved(mut self, id: TopicId, parent: Option<TopicId>) -> Self {
        self.parents.insert(id, parent);
        self
    }

    #[must_use]
    pub fn without(mut self, id: TopicId) -> Self {
        self.parents.remove(&id);
        self
    }

    fn height_within(&self, id: TopicId, budget: usize) -> usize {
        let below = match budget {
            0 => 0,
            left => self
                .children(id)
                .into_iter()
                .filter(|child| !self.entities.contains(child))
                .map(|child| self.height_within(child, left - 1))
                .fold(0, usize::max),
        };
        below + 1
    }

    fn parent(&self, id: TopicId) -> Option<TopicId> {
        self.parents.get(&id).copied().flatten()
    }
}

pub async fn page_owner(
    topics: &dyn TopicStore,
    topic: &Topic,
) -> Result<Option<Topic>, TopicError> {
    match topic.kind {
        TopicKind::Work => root_of(topics, topic).await.map(Some),
        TopicKind::Entity => Ok(Some(topic.clone())),
        TopicKind::Type => Ok(None),
    }
}

pub async fn root_of(topics: &dyn TopicStore, topic: &Topic) -> Result<Topic, TopicError> {
    let mut current = topic.clone();
    for _ in 0..MAX_DEPTH {
        let Some(parent) = current.parent else {
            return Ok(current);
        };
        current =
            topics
                .find(&TopicKey::Id(parent))
                .await?
                .ok_or_else(|| TopicError::NotFound {
                    key: parent.to_string(),
                })?;
    }
    Ok(current)
}

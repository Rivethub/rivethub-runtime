use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

use super::builder::HeldEntity;
use super::builder::WikiBuilderStore;
use super::builder::WikiSummary;
use super::page::WikiPatch;
use crate::cleanup::EntityType;
use crate::topics::Alias;
use crate::topics::NewTopic;
use crate::topics::Redirect;
use crate::topics::Slug;
use crate::topics::Topic;
use crate::topics::TopicChanges;
use crate::topics::TopicError;
use crate::topics::TopicId;
use crate::topics::TopicKind;
use crate::topics::TopicName;
use crate::topics::TopicPageError;
use crate::topics::TopicStore;
use crate::topics::TopicTree;

mod name;

use name::entity_name;

const SESSIONS_TO_CREATE: usize = 2;
const PATH_SEPARATOR: &str = " › ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    pub entity: Topic,
    pub patches: Vec<WikiPatch>,
    pub earlier: Vec<Uuid>,
    pub held: Vec<HeldEntity>,
}

impl Filed {
    fn of(entity: Topic, summary: &WikiSummary, patch: WikiPatch, held: Vec<HeldEntity>) -> Self {
        let own = source(summary);
        let others: Vec<&HeldEntity> = held
            .iter()
            .filter(|mention| mention.source != own)
            .collect();
        let mut patches: Vec<WikiPatch> =
            others.iter().map(|mention| mention.patch.clone()).collect();
        patches.push(patch);
        Self {
            entity,
            patches,
            earlier: others
                .iter()
                .map(|mention| mention.summary_id)
                .filter(|id| *id != summary.id)
                .collect(),
            held,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    entities: Vec<Topic>,
    types: Vec<Topic>,
    redirects: Vec<Redirect>,
}

impl Catalog {
    pub async fn load(topics: &dyn TopicStore) -> Result<Self, TopicPageError> {
        let mut entities = Vec::new();
        let mut types = Vec::new();
        for listing in topics.list().await? {
            match listing.topic.kind {
                TopicKind::Entity => entities.push(listing.topic),
                TopicKind::Type => types.push(listing.topic),
                TopicKind::Work => {}
            }
        }
        Ok(Self {
            entities,
            types,
            redirects: topics.redirects().await?,
        })
    }

    #[must_use]
    pub fn type_paths(&self) -> Vec<String> {
        let tree = TopicTree::of(&self.types);
        let mut paths: Vec<String> = self
            .types
            .iter()
            .map(|kind| {
                let mut names: Vec<&str> = tree
                    .ancestors(kind.id)
                    .filter_map(|above| self.types.iter().find(|known| known.id == above))
                    .map(|above| above.name.as_str())
                    .collect();
                names.reverse();
                names.push(kind.name.as_str());
                names.join(PATH_SEPARATOR)
            })
            .collect();
        paths.sort();
        paths
    }

    pub async fn file(
        &mut self,
        topics: &dyn TopicStore,
        wiki: &dyn WikiBuilderStore,
        summary: &WikiSummary,
        patch: WikiPatch,
        now: DateTime<Utc>,
    ) -> Result<Option<Filed>, TopicPageError> {
        let kind = patch
            .entity_type
            .as_deref()
            .and_then(|named| self.type_named(named))
            .map(|kind| kind.id);
        let names = names(&patch);
        if let Some(entity) = self.known(&names, kind).cloned() {
            let entity = self.learn(topics, entity, &patch, now).await?;
            let keys: BTreeSet<Slug> = names.into_iter().chain(self.keys(&entity)).collect();
            let held = held_under(wiki, &keys).await?;
            return Ok(Some(Filed::of(entity, summary, patch, held)));
        }
        for name in &names {
            if topics.tombstoned(TopicKind::Entity, name).await? {
                return Ok(None);
            }
        }
        let Some(key) = self.name_of(&patch, kind).and_then(Slug::of_heading) else {
            return Ok(None);
        };
        wiki.hold_entity(HeldEntity {
            key,
            source: source(summary),
            summary_id: summary.id,
            patch: patch.clone(),
            at: now,
        })
        .await?;
        let held = held_under(wiki, &names).await?;
        let sources: BTreeSet<&str> = held.iter().map(|mention| mention.source.as_str()).collect();
        if sources.len() < SESSIONS_TO_CREATE {
            return Ok(None);
        }
        let Some(parent) = held
            .iter()
            .rev()
            .filter_map(|mention| mention.patch.entity_type.as_deref())
            .find_map(|named| self.type_named(named))
            .map(|kind| kind.id)
        else {
            return Ok(None);
        };
        let Some(name) = held
            .iter()
            .rev()
            .find_map(|mention| self.name_of(&mention.patch, Some(parent)))
        else {
            return Ok(None);
        };
        let patches: Vec<&WikiPatch> = held.iter().map(|mention| &mention.patch).collect();
        let aliases = self.new_aliases(name, BTreeSet::new(), parent, &patches)?;
        let created = topics
            .create(Topic::create(
                TopicId::random(),
                NewTopic {
                    kind: TopicKind::Entity,
                    name: TopicName::new(name)?,
                    aliases,
                    parent: Some(parent),
                    directory: None,
                    owner: summary.owner.clone(),
                },
                now,
            )?)
            .await?;
        self.entities.push(created.clone());
        Ok(Some(Filed::of(created, summary, patch, held)))
    }

    fn known(&self, names: &BTreeSet<Slug>, kind: Option<TopicId>) -> Option<&Topic> {
        let compacted: BTreeSet<String> = names.iter().map(compact).collect();
        self.entities
            .iter()
            .filter_map(|entity| {
                let exact = self.keys(entity).any(|key| names.contains(&key));
                let close = self
                    .keys(entity)
                    .any(|key| compacted.contains(&compact(&key)));
                let other_type = kind.is_none_or(|kind| entity.parent != Some(kind));
                (exact || close).then_some(((other_type, !exact), entity))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, entity)| entity)
    }

    fn name_of<'patch>(
        &self,
        patch: &'patch WikiPatch,
        kind: Option<TopicId>,
    ) -> Option<&'patch str> {
        entity_name(&patch.title, self.entity_type(kind))
    }

    fn entity_type(&self, kind: Option<TopicId>) -> Option<EntityType> {
        let kind = self.types.iter().find(|known| Some(known.id) == kind)?;
        EntityType::named(kind.name.as_str())
    }

    async fn learn(
        &mut self,
        topics: &dyn TopicStore,
        entity: Topic,
        patch: &WikiPatch,
        now: DateTime<Utc>,
    ) -> Result<Topic, TopicPageError> {
        let Some(kind) = entity.parent else {
            return Ok(entity);
        };
        let own: BTreeSet<Slug> = self.keys(&entity).collect();
        let learned = self.new_aliases(entity.name.as_str(), own, kind, &[patch])?;
        if learned.is_empty() {
            return Ok(entity);
        }
        let changes = TopicChanges {
            aliases: Some(entity.aliases.iter().cloned().chain(learned).collect()),
            ..TopicChanges::default()
        };
        let updated = topics.update(entity.id, changes, now).await?;
        if let Some(slot) = self
            .entities
            .iter_mut()
            .find(|known| known.id == updated.id)
        {
            updated.clone_into(slot);
        }
        Ok(updated)
    }

    fn new_aliases(
        &self,
        name: &str,
        mut seen: BTreeSet<Slug>,
        kind: TopicId,
        patches: &[&WikiPatch],
    ) -> Result<Vec<Alias>, TopicError> {
        let kind = self.entity_type(Some(kind));
        seen.extend(Slug::of_heading(name));
        let mut aliases = Vec::new();
        let texts = patches.iter().flat_map(|patch| {
            std::iter::once(patch.title.as_str()).chain(patch.aliases.iter().map(AsRef::as_ref))
        });
        for text in texts {
            let Some(alias) = entity_name(text, kind) else {
                continue;
            };
            let Some(key) = Slug::of_heading(alias) else {
                continue;
            };
            if !self.claimed(&key) && seen.insert(key) {
                aliases.push(Alias::new(alias)?);
            }
        }
        Ok(aliases)
    }

    fn claimed(&self, key: &Slug) -> bool {
        self.entities
            .iter()
            .any(|entity| self.keys(entity).any(|known| known == *key))
    }

    fn type_named(&self, named: &str) -> Option<&Topic> {
        let path = named
            .split(['›', '>', '/'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(Slug::of_heading)
            .collect::<Option<Vec<Slug>>>()?;
        let (leaf, above) = path.split_last()?;
        let tree = TopicTree::of(&self.types);
        let mut found = self.types.iter().filter(|kind| {
            self.named_by(kind, leaf) && (above.is_empty() || self.under(&tree, kind, above))
        });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn under(&self, tree: &TopicTree, kind: &Topic, path: &[Slug]) -> bool {
        let ancestors: Vec<&Topic> = tree
            .ancestors(kind.id)
            .filter_map(|above| self.types.iter().find(|known| known.id == above))
            .collect();
        ancestors.len() == path.len()
            && ancestors
                .iter()
                .zip(path.iter().rev())
                .all(|(above, slug)| self.named_by(above, slug))
    }

    fn named_by(&self, topic: &Topic, slug: &Slug) -> bool {
        self.keys(topic).any(|key| key == *slug)
    }

    fn keys<'a>(&'a self, topic: &'a Topic) -> impl Iterator<Item = Slug> + 'a {
        std::iter::once(topic.slug.clone())
            .chain(
                topic
                    .aliases
                    .iter()
                    .filter_map(|alias| Slug::of_heading(alias.as_str())),
            )
            .chain(
                self.redirects
                    .iter()
                    .filter(|redirect| redirect.topic == topic.id)
                    .map(|redirect| redirect.slug.clone()),
            )
    }
}

fn names(patch: &WikiPatch) -> BTreeSet<Slug> {
    [patch.title.as_str(), patch.slug.as_ref()]
        .into_iter()
        .chain(entity_name(&patch.title, None))
        .chain(patch.aliases.iter().map(AsRef::as_ref))
        .filter_map(Slug::of_heading)
        .collect()
}

fn compact(slug: &Slug) -> String {
    slug.as_str().replace('-', "")
}

async fn held_under(
    wiki: &dyn WikiBuilderStore,
    keys: &BTreeSet<Slug>,
) -> Result<Vec<HeldEntity>, TopicPageError> {
    let mut held: Vec<HeldEntity> = Vec::new();
    for key in keys {
        for mention in wiki.held_entities(key).await? {
            if !held.contains(&mention) {
                held.push(mention);
            }
        }
    }
    held.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.source.cmp(&right.source))
    });
    Ok(held)
}

fn source(summary: &WikiSummary) -> String {
    match (&summary.session_key, summary.conversation_id) {
        (Some(key), _) => key.clone(),
        (None, Some(conversation)) => conversation.to_string(),
        (None, None) => summary.id.to_string(),
    }
}

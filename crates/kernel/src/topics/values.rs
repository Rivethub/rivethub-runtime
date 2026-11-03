use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

use super::TopicError;
use crate::owner::Owner;
use crate::tenancy::Visibility;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct TopicId(Uuid);

impl TopicId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for TopicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TopicId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slug(String);

impl Slug {
    pub fn fold(text: &str) -> Result<Self, TopicError> {
        let folded = fold(text);
        if folded.is_empty() {
            Err(TopicError::EmptySlug {
                text: text.to_owned(),
            })
        } else {
            Ok(Self(folded))
        }
    }

    #[must_use]
    pub fn of_heading(text: &str) -> Option<Self> {
        let folded = fold(text);
        (!folded.is_empty()).then_some(Self(folded))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut gap = false;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if gap && !folded.is_empty() {
                folded.push('-');
            }
            gap = false;
            folded.push(ch);
        } else {
            gap = true;
        }
    }
    folded
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicName(String);

impl TopicName {
    pub fn new(text: &str) -> Result<Self, TopicError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Err(TopicError::EmptyName)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias(String);

impl Alias {
    pub fn new(text: &str) -> Result<Self, TopicError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Err(TopicError::EmptyAlias)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    #[must_use]
    pub fn of_name(name: &TopicName) -> Self {
        Self(name.as_str().to_owned())
    }
}

#[must_use]
pub fn distinct_aliases(aliases: Vec<Alias>) -> Vec<Alias> {
    let mut seen = BTreeSet::new();
    let mut kept: Vec<Alias> = aliases
        .into_iter()
        .filter(|alias| seen.insert(alias.key()))
        .collect();
    kept.sort_by_key(Alias::key);
    kept
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum TopicKind {
    #[default]
    Work,
    Type,
    Entity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum RootKind {
    Local,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    Local { node: String, path: String },
    Shared { path: String },
}

impl Root {
    #[must_use]
    pub fn kind(&self) -> RootKind {
        match self {
            Self::Local { .. } => RootKind::Local,
            Self::Shared { .. } => RootKind::Shared,
        }
    }

    #[must_use]
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::Local { node, .. } => Some(node),
            Self::Shared { .. } => None,
        }
    }

    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Local { path, .. } | Self::Shared { path } => path,
        }
    }

    #[must_use]
    pub fn from_parts(kind: RootKind, node: Option<String>, path: String) -> Option<Self> {
        match (kind, node) {
            (RootKind::Local, Some(node)) => Some(Self::Local { node, path }),
            (RootKind::Shared, None) => Some(Self::Shared { path }),
            (RootKind::Local, None) | (RootKind::Shared, Some(_)) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic {
    pub id: TopicId,
    pub kind: TopicKind,
    pub slug: Slug,
    pub name: TopicName,
    pub aliases: Vec<Alias>,
    pub parent: Option<TopicId>,
    pub directory: Option<Root>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub owner: Option<Owner>,
    pub visibility: Visibility,
    pub visibility_mark: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTopic {
    pub kind: TopicKind,
    pub name: TopicName,
    pub aliases: Vec<Alias>,
    pub parent: Option<TopicId>,
    pub directory: Option<Root>,
    pub owner: Option<Owner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Change<T> {
    #[default]
    Keep,
    Set(T),
    Clear,
}

impl<T> Change<T> {
    fn applied_to(self, current: Option<T>) -> Option<T> {
        match self {
            Self::Keep => current,
            Self::Set(value) => Some(value),
            Self::Clear => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopicChanges {
    pub name: Option<TopicName>,
    pub aliases: Option<Vec<Alias>>,
    pub parent: Change<TopicId>,
    pub directory: Change<Root>,
}

impl Topic {
    pub fn create(id: TopicId, new: NewTopic, now: DateTime<Utc>) -> Result<Self, TopicError> {
        if new.kind == TopicKind::Entity && new.parent.is_none() {
            return Err(TopicError::EntityWithoutType);
        }
        Ok(Self {
            id,
            kind: new.kind,
            slug: Slug::fold(new.name.as_str())?,
            name: new.name,
            aliases: distinct_aliases(new.aliases),
            parent: new.parent,
            directory: new.directory,
            created_at: now,
            updated_at: now,
            closed_at: None,
            owner: new.owner,
            visibility: Visibility::Private,
            visibility_mark: None,
        })
    }

    #[must_use]
    pub fn closed(mut self, at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        self.closed_at = at;
        self.updated_at = now;
        self
    }

    pub fn accepts_sessions(&self) -> Result<(), TopicError> {
        if self.kind == TopicKind::Work {
            Ok(())
        } else {
            Err(TopicError::NotWork {
                slug: self.slug.clone(),
            })
        }
    }

    pub fn check_delete(&self, has_subtopics: bool) -> Result<(), TopicError> {
        if has_subtopics {
            return Err(TopicError::SubtopicsRemain {
                slug: self.slug.clone(),
            });
        }
        Ok(())
    }

    pub fn apply(mut self, changes: TopicChanges, now: DateTime<Utc>) -> Result<Self, TopicError> {
        if let Some(name) = changes.name {
            self.slug = Slug::fold(name.as_str())?;
            let former = std::mem::replace(&mut self.name, name);
            if changes.aliases.is_none() {
                self.keep_alias(&former);
            }
        }
        if let Some(aliases) = changes.aliases {
            self.aliases = distinct_aliases(aliases);
        }
        self.parent = changes.parent.applied_to(self.parent);
        if self.kind == TopicKind::Entity && self.parent.is_none() {
            return Err(TopicError::EntityWithoutType);
        }
        self.directory = changes.directory.applied_to(self.directory);
        self.updated_at = now;
        Ok(self)
    }

    fn keep_alias(&mut self, former: &TopicName) {
        let alias = Alias::of_name(former);
        if alias.key() != Alias::of_name(&self.name).key() {
            self.aliases.push(alias);
            self.aliases = distinct_aliases(std::mem::take(&mut self.aliases));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicListing {
    pub topic: Topic,
    pub session_count: u64,
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub slug: Slug,
    pub topic: TopicId,
}

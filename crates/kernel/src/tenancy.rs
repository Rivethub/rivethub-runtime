use jiff::Timestamp;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

use async_trait::async_trait;
use values::SessionKey;

use crate::BoxError;
use crate::owner::Owner;
use crate::topics::TopicId;

pub const DEFAULT_SPACE: &str = "household";
const LEGACY_DEFAULT_SPACE_NAME: &str = "Household";
pub const ONE_MEMBER_SPACE_NAME: &str = "This device";
pub const SHARED_SPACE_NAME: &str = "This hub";
const PRIVATE: &str = "private";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpaceId(String);

impl SpaceId {
    pub fn new(id: String) -> Result<Self, TenancyError> {
        let valid = !id.is_empty()
            && id != PRIVATE
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if valid {
            Ok(Self(id))
        } else {
            Err(TenancyError::InvalidSpace(id))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpaceKind {
    Device,
    Team,
    Compartment,
}

impl SpaceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "household",
            Self::Team => "team",
            Self::Compartment => "compartment",
        }
    }

    pub fn parse(text: &str) -> Result<Self, TenancyError> {
        match text {
            "household" | "device" => Ok(Self::Device),
            "team" => Ok(Self::Team),
            "compartment" => Ok(Self::Compartment),
            other => Err(TenancyError::InvalidKind(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub id: SpaceId,
    pub kind: SpaceKind,
    pub name: String,
    pub members: Vec<Owner>,
}

impl Space {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.id.as_str() == DEFAULT_SPACE && self.kind == SpaceKind::Device
    }

    #[must_use]
    pub fn synced_name<'a>(&'a self, stored_default_name: Option<&'a str>) -> &'a str {
        if self.is_default()
            && let Some(stored) = stored_default_name
            && stored != LEGACY_DEFAULT_SPACE_NAME
            && stored != ONE_MEMBER_SPACE_NAME
            && stored != SHARED_SPACE_NAME
        {
            return stored;
        }
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Visibility {
    Private,
    Space(SpaceId),
}

impl Visibility {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Private => PRIVATE,
            Self::Space(space) => space.as_str(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, TenancyError> {
        if text == PRIVATE {
            Ok(Self::Private)
        } else {
            SpaceId::new(text.to_owned()).map(Self::Space)
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    principal: Owner,
    spaces: Vec<SpaceId>,
}

impl Viewer {
    #[must_use]
    pub fn new(principal: Owner, spaces: Vec<SpaceId>) -> Self {
        Self { principal, spaces }
    }

    #[must_use]
    pub fn principal(&self) -> &Owner {
        &self.principal
    }

    #[must_use]
    pub fn spaces(&self) -> &[SpaceId] {
        &self.spaces
    }

    pub fn may_share(&self, visibility: &Visibility) -> Result<(), TenancyError> {
        match visibility {
            Visibility::Private => Ok(()),
            Visibility::Space(space) if self.spaces.contains(space) => Ok(()),
            Visibility::Space(space) => Err(TenancyError::NotMember {
                principal: self.principal.as_str().to_owned(),
                space: space.as_str().to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Viewer(Viewer),
    Maintenance,
    Closed,
}

tokio::task_local! {
    static SCOPE: Scope;
}

impl Scope {
    pub async fn run<F: Future>(self, work: F) -> F::Output {
        SCOPE.scope(self, work).await
    }

    pub fn enter<R>(self, work: impl FnOnce() -> R) -> R {
        SCOPE.sync_scope(self, work)
    }

    pub fn carry<F: Future>(work: F) -> impl Future<Output = F::Output> {
        let scope = Self::current().unwrap_or(Self::Closed);
        scope.run(work)
    }

    #[must_use]
    pub fn current() -> Option<Self> {
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let current = SCOPE.try_with(Self::clone).ok();
        current
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilityDefault {
    pub visibility: Visibility,
    pub since: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVisibility {
    pub owner: Option<Owner>,
    pub mark: Option<Visibility>,
    pub effective: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicVisibility {
    pub topic: TopicId,
    pub mark: Option<Visibility>,
    pub effective: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TenancyError {
    #[error("invalid space id {0:?}")]
    InvalidSpace(String),
    #[error("unknown space kind {0:?}")]
    InvalidKind(String),
    #[error("{principal} is not a member of space {space}")]
    NotMember { principal: String, space: String },
}

#[derive(Debug, thiserror::Error)]
pub enum TenancyStoreError {
    #[error(transparent)]
    Invalid(#[from] TenancyError),
    #[error("{0} belongs to someone else")]
    NotOwner(String),
    #[error("{0} was not found")]
    NotFound(String),
    #[error("{0}")]
    CatalogConflict(String),
    #[error("tenancy store is unavailable")]
    Unavailable(#[source] BoxError),
}

#[async_trait]
pub trait TenancyStore: Send + Sync {
    async fn sync_spaces(&self, node: &str, spaces: &[Space]) -> Result<(), TenancyStoreError>;

    async fn default_visibility(
        &self,
        user: &Owner,
    ) -> Result<Option<VisibilityDefault>, TenancyStoreError>;

    async fn set_default_visibility(
        &self,
        user: &Owner,
        visibility: &Visibility,
        now: Timestamp,
    ) -> Result<VisibilityDefault, TenancyStoreError>;

    fn row_secured(&self) -> bool {
        false
    }

    async fn session_visibility(
        &self,
        viewer: &Viewer,
        sessions: &[SessionKey],
    ) -> Result<BTreeMap<SessionKey, SessionVisibility>, TenancyStoreError>;

    async fn mark_session(
        &self,
        viewer: &Viewer,
        session: &SessionKey,
        mark: Option<&Visibility>,
        now: Timestamp,
    ) -> Result<SessionVisibility, TenancyStoreError>;

    async fn mark_topic(
        &self,
        viewer: &Viewer,
        topic: TopicId,
        mark: Option<&Visibility>,
    ) -> Result<TopicVisibility, TenancyStoreError>;

    async fn viewer_of(&self, owner: &Owner) -> Result<Viewer, TenancyStoreError>;

    async fn conversation_owner(
        &self,
        conversation: uuid::Uuid,
    ) -> Result<Option<Owner>, TenancyStoreError>;
}

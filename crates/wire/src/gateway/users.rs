use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_OWNER_ID: &str = "owner";

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UsersRegistry {
    pub owner_user_id: String,
    pub unmapped_is_owner: bool,
    pub local_stores: Option<bool>,
    pub users: BTreeMap<String, UserRecord>,
    #[serde(default)]
    pub spaces: Option<BTreeMap<String, crate::gateway::tenancy::SpaceConfig>>,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl Default for UsersRegistry {
    fn default() -> Self {
        Self {
            owner_user_id: DEFAULT_OWNER_ID.to_owned(),
            unmapped_is_owner: false,
            local_stores: None,
            users: BTreeMap::new(),
            spaces: None,
        }
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub id: Option<String>,
    pub devices: Option<Vec<String>>,
    pub tokens: Option<Vec<UserToken>>,
    pub pg_url: Option<String>,
    pub env_file: Option<String>,
    pub persona: Option<String>,
    #[serde(default)]
    pub admin: Option<bool>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
pub enum TokenScope {
    #[serde(rename = "full")]
    #[strum(serialize = "full")]
    Full,
    #[serde(rename = "drops:write")]
    #[strum(serialize = "drops:write")]
    DropsWrite,
    #[serde(rename = "memory:import")]
    #[strum(serialize = "memory:import")]
    MemoryImport,
}

impl TokenScope {
    const ALL: [Self; 3] = [Self::Full, Self::DropsWrite, Self::MemoryImport];

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.to_string() == name)
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::DropsWrite => 2,
            Self::MemoryImport => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenScopes(u8);

impl TokenScopes {
    pub const FULL: Self = Self(TokenScope::Full.bit());

    #[must_use]
    pub fn of(scopes: &[TokenScope]) -> Option<Self> {
        let bits = scopes.iter().fold(0, |bits, scope| bits | scope.bit());
        (bits != 0).then_some(Self(bits))
    }

    #[must_use]
    pub const fn allows(self, needed: TokenScope) -> bool {
        self.0 & (TokenScope::Full.bit() | needed.bit()) != 0
    }

    #[must_use]
    pub fn listed(self) -> Vec<TokenScope> {
        TokenScope::ALL
            .into_iter()
            .filter(|scope| self.0 & scope.bit() != 0)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum UserToken {
    Digest(String),
    Scoped(ScopedToken),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopedToken {
    pub digest: String,
    pub scopes: Vec<TokenScope>,
}

impl UserToken {
    #[must_use]
    pub fn digest(&self) -> &str {
        match self {
            Self::Digest(digest) | Self::Scoped(ScopedToken { digest, .. }) => digest,
        }
    }

    #[must_use]
    pub fn scopes(&self) -> Option<TokenScopes> {
        match self {
            Self::Digest(_) => Some(TokenScopes::FULL),
            Self::Scoped(ScopedToken { scopes, .. }) => TokenScopes::of(scopes),
        }
    }
}

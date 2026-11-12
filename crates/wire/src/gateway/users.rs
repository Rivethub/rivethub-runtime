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

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "B11: the users registry default requires the named owner id; derive would use an empty id"
    )
)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum TokenScope {
    #[serde(rename = "full")]
    Full,
    #[serde(rename = "drops:write")]
    DropsWrite,
}

impl TokenScope {
    #[must_use]
    pub const fn allows(self, needed: Self) -> bool {
        matches!(
            (self, needed),
            (Self::Full, _) | (Self::DropsWrite, Self::DropsWrite)
        )
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
    pub fn scope(&self) -> Option<TokenScope> {
        match self {
            Self::Digest(_) => Some(TokenScope::Full),
            Self::Scoped(ScopedToken { scopes, .. }) => scopes
                .iter()
                .copied()
                .reduce(|held, scope| if held.allows(scope) { held } else { scope }),
        }
    }
}

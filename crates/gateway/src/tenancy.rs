use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use headers::Authorization;
use headers::HeaderMapExt as _;
use headers::authorization::Bearer;
use kernel::owner::Owner;
use kernel::tenancy::Space;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UserId(Owner);

impl UserId {
    pub fn new(id: String) -> Result<Self, ApiError> {
        Owner::new(id)
            .map(Self)
            .map_err(|_| ApiError::BadRequest("invalid user id".into()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn owner(&self) -> &Owner {
        &self.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tenancy {
    local: Option<UserId>,
    tokens: Vec<([u8; 32], UserId)>,
    spaces: Vec<Space>,
    admins: Vec<UserId>,
}

impl Tenancy {
    #[must_use]
    pub fn with_spaces(mut self, spaces: Vec<Space>) -> Self {
        self.spaces = spaces;
        self
    }

    #[must_use]
    pub fn with_admin(mut self, user: UserId) -> Self {
        if !self.admins.contains(&user) {
            self.admins.push(user);
        }
        self
    }

    #[must_use]
    pub fn spaces(&self) -> &[Space] {
        &self.spaces
    }

    #[must_use]
    pub fn viewer(&self, user: &UserId) -> Viewer {
        let spaces = self
            .spaces
            .iter()
            .filter(|space| space.members.contains(user.owner()))
            .map(|space| space.id.clone())
            .collect();
        Viewer::new(user.owner().clone(), spaces)
    }

    #[must_use]
    pub fn local_user(&self) -> Option<&UserId> {
        self.local.as_ref()
    }

    pub fn local(user: UserId) -> Self {
        Self {
            local: Some(user),
            ..Self::default()
        }
    }

    pub fn with_token(mut self, token: &str, user: UserId) -> Result<Self, ApiError> {
        if token.is_empty() {
            return Err(ApiError::BadRequest("empty user token".into()));
        }
        self.tokens
            .push((Sha256::digest(token.as_bytes()).into(), user));
        Ok(self)
    }

    pub fn with_token_digest(mut self, digest: &str, user: UserId) -> Result<Self, ApiError> {
        let mut secret = [0_u8; 32];
        hex::decode_to_slice(digest.trim(), &mut secret).map_err(|_| {
            ApiError::BadRequest("a users-file token must be the hex SHA-256 of the secret".into())
        })?;
        if self.tokens.iter().any(|(known, _)| *known == secret) {
            return Err(ApiError::BadRequest("duplicate users-file token".into()));
        }
        self.tokens.push((secret, user));
        Ok(self)
    }

    pub(crate) fn owner(&self, user: &UserId) -> Result<(), ApiError> {
        if self.local.as_ref() == Some(user) || self.admins.contains(user) {
            Ok(())
        } else {
            Err(ApiError::OwnerRequired)
        }
    }

    pub(crate) fn resolve_bearer(&self, token: &str) -> Result<UserId, ApiError> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for (secret, user) in &self.tokens {
            if bool::from(secret.ct_eq(&digest)) {
                found = Some(user.clone());
            }
        }
        found.ok_or(ApiError::Unauthorized)
    }
}

impl FromRequestParts<std::sync::Arc<crate::Gateway>> for UserId {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        gateway: &std::sync::Arc<crate::Gateway>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(caller(gateway, parts))
    }
}

pub(crate) fn caller(gateway: &crate::Gateway, parts: &Parts) -> Result<UserId, ApiError> {
    if parts
        .extensions
        .get::<crate::plugins::PluginCaller>()
        .is_some()
    {
        return gateway
            .tenancy
            .local_user()
            .cloned()
            .ok_or(ApiError::Unauthorized);
    }
    let presented = parts
        .headers
        .typed_try_get::<Authorization<Bearer>>()
        .map_err(|_| ApiError::Unauthorized)?
        .ok_or(ApiError::Unauthorized)?;
    let token = presented.token();
    if crate::auth::direct_local(parts)
        && gateway
            .local_owner()
            .is_some_and(|owner| owner.verify(token))
    {
        return gateway
            .tenancy
            .local_user()
            .cloned()
            .ok_or(ApiError::Unauthorized);
    }
    gateway.tenancy.resolve_bearer(token)
}

#[cfg(test)]
mod tests;

pub(crate) fn shared_with(visibility: &Visibility) -> wire::gateway::session_catalog::SharedWith {
    use wire::gateway::session_catalog::SharedWith;
    match visibility {
        Visibility::Private => SharedWith::Private,
        Visibility::Space(space) => SharedWith::Space {
            space: space.as_str().to_owned(),
        },
    }
}

use axum::extract::FromRequestParts;
use axum::http::Method;
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
use wire::gateway::users::{TokenScope, UserToken};

use crate::auth_event::AuthEvent;
use crate::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    tokens: Vec<([u8; 32], UserId, TokenScope)>,
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
        self.tokens.push((
            Sha256::digest(token.as_bytes()).into(),
            user,
            TokenScope::Full,
        ));
        Ok(self)
    }

    pub fn with_users_file_token(
        mut self,
        token: &UserToken,
        user: UserId,
    ) -> Result<Self, ApiError> {
        let mut secret = [0_u8; 32];
        hex::decode_to_slice(token.digest().trim(), &mut secret).map_err(|_| {
            ApiError::BadRequest("a users-file token must be the hex SHA-256 of the secret".into())
        })?;
        let scope = token.scope().ok_or_else(|| {
            ApiError::BadRequest("a scoped users-file token must name at least one scope".into())
        })?;
        if self.tokens.iter().any(|(known, ..)| *known == secret) {
            return Err(ApiError::BadRequest("duplicate users-file token".into()));
        }
        self.tokens.push((secret, user, scope));
        Ok(self)
    }

    pub(crate) fn owner(&self, user: &UserId) -> Result<(), ApiError> {
        if self.local.as_ref() == Some(user) || self.admins.contains(user) {
            Ok(())
        } else {
            Err(ApiError::OwnerRequired)
        }
    }

    pub(crate) fn user_for_token(&self, token: &str) -> Option<(UserId, TokenScope)> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for (secret, user, scope) in &self.tokens {
            if bool::from(secret.ct_eq(&digest)) {
                found = Some((user.clone(), *scope));
            }
        }
        found
    }

    pub(crate) fn resolve_bearer(&self, token: &str, parts: &Parts) -> Result<UserId, ApiError> {
        let (user, scope) = self.user_for_token(token).ok_or(ApiError::InvalidToken)?;
        if scope.allows(TokenScope::Full) || drops_scoped(&parts.method, parts.uri.path()) {
            Ok(user)
        } else {
            Err(ApiError::InsufficientScope)
        }
    }
}

fn drops_scoped(method: &Method, path: &str) -> bool {
    crate::drops::is_upload(method, path)
        || crate::drops::is_revoke(method, path)
        || (method == Method::GET && path == "/api/me")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Credential {
    Plugin,
    OwnerToken,
    Bearer,
    Session { auth_time: i64 },
}

impl Credential {
    pub(crate) fn is_local_owner(self) -> bool {
        matches!(self, Self::OwnerToken | Self::Session { .. })
    }
}

pub(crate) fn caller(gateway: &crate::Gateway, parts: &Parts) -> Result<UserId, ApiError> {
    authenticate(gateway, parts).map(|(user, _)| user)
}

pub(crate) fn authenticate(
    gateway: &crate::Gateway,
    parts: &Parts,
) -> Result<(UserId, Credential), ApiError> {
    let local = || {
        gateway
            .tenancy
            .local_user()
            .cloned()
            .ok_or(ApiError::Unauthorized)
    };
    if parts
        .extensions
        .get::<crate::plugins::PluginCaller>()
        .is_some()
    {
        return Ok((local()?, Credential::Plugin));
    }
    let Some(presented) = parts
        .headers
        .typed_try_get::<Authorization<Bearer>>()
        .map_err(|_| ApiError::Unauthorized)?
    else {
        let auth_time = session(parts)?;
        return Ok((local()?, Credential::Session { auth_time }));
    };
    let token = presented.token();
    let peer = crate::login::peer_ip(parts);
    if gateway
        .local_owner()
        .is_some_and(|owner| owner.verify(token))
    {
        if crate::auth::direct_local(parts) {
            return Ok((local()?, Credential::OwnerToken));
        }
        gateway
            .auth_log()
            .emit(AuthEvent::ForwardedTokenRefused, peer);
        return Err(ApiError::Unauthorized);
    }
    gateway
        .tenancy
        .resolve_bearer(token, parts)
        .map(|user| (user, Credential::Bearer))
        .inspect_err(|_| gateway.auth_log().emit(AuthEvent::BearerRejected, peer))
}

pub(crate) fn owner_token_presented(gateway: &crate::Gateway, parts: &Parts) -> bool {
    parts
        .headers
        .typed_get::<Authorization<Bearer>>()
        .is_some_and(|presented| {
            crate::auth::direct_local(parts)
                && gateway
                    .local_owner()
                    .is_some_and(|owner| owner.verify(presented.token()))
        })
}

fn session(parts: &Parts) -> Result<i64, ApiError> {
    let login = parts
        .extensions
        .get::<crate::login::BrowserLogin>()
        .filter(|_| crate::auth::direct_local(parts))
        .ok_or(ApiError::Unauthorized)?;
    if crate::auth::csrf_exempt(parts) {
        return Ok(login.auth_time);
    }
    let sent: [u8; 32] = parts
        .headers
        .get(crate::login::CSRF_HEADER)
        .map(|value| Sha256::digest(value.as_bytes()).into())
        .ok_or(ApiError::Unauthorized)?;
    if bool::from(sent.ct_eq(&login.csrf)) {
        Ok(login.auth_time)
    } else {
        Err(ApiError::Unauthorized)
    }
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

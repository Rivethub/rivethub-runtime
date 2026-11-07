mod certificate;

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
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

#[derive(Debug, Clone)]
pub enum PeerCertificate {
    Device {
        user: Option<String>,
        device: String,
    },
    Node {
        node: String,
    },
    Agent {
        node: String,
        agent: String,
    },
    Unrecognized,
}

impl PeerCertificate {
    pub(crate) fn verified_principal(
        &self,
    ) -> Result<Option<kernel::ca::Principal>, kernel::ca::CaError> {
        let principal = match self {
            Self::Node { node } => kernel::ca::Principal::Node { node: node.clone() },
            Self::Agent { node, agent } => kernel::ca::Principal::Agent {
                node: node.clone(),
                agent: agent.clone(),
            },
            Self::Device {
                user: Some(user),
                device,
            } => kernel::ca::Principal::Device {
                user: user.clone(),
                device: device.clone(),
            },
            Self::Device { user: None, .. } | Self::Unrecognized => return Ok(None),
        };
        principal.spiffe_uri()?;
        Ok(Some(principal))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tenancy {
    local: Option<UserId>,
    tokens: Vec<([u8; 32], UserId)>,
    devices: BTreeMap<String, UserId>,
    unmapped_device_owner: Option<UserId>,
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

    pub fn with_device(mut self, name: &str, user: UserId) -> Result<Self, ApiError> {
        let device = match name.strip_prefix("device:") {
            Some(name) => name,
            None => name,
        };
        if device.is_empty() || device.chars().any(char::is_control) {
            return Err(ApiError::BadRequest("invalid registry device name".into()));
        }
        if self.devices.insert(device.to_owned(), user).is_some() {
            return Err(ApiError::BadRequest(
                "duplicate registry device name".into(),
            ));
        }
        Ok(self)
    }

    #[must_use]
    pub fn with_unmapped_device_owner(mut self, owner: UserId) -> Self {
        self.unmapped_device_owner = Some(owner);
        self
    }

    pub(crate) fn owner(&self, user: &UserId) -> Result<(), ApiError> {
        if self.local.as_ref() == Some(user) || self.admins.contains(user) {
            Ok(())
        } else {
            Err(ApiError::OwnerRequired)
        }
    }

    pub(crate) fn device_user(&self, peer: &PeerCertificate) -> Result<UserId, ApiError> {
        let (user, device) = match peer {
            PeerCertificate::Device { user, device } => (user, device),
            PeerCertificate::Node { .. }
            | PeerCertificate::Agent { .. }
            | PeerCertificate::Unrecognized => return Err(ApiError::Unauthorized),
        };
        let mapped = self
            .devices
            .get(device)
            .or(self.unmapped_device_owner.as_ref());
        let mapped = mapped.ok_or(ApiError::Unauthorized)?;
        if user
            .as_ref()
            .is_some_and(|claimed| claimed != mapped.as_str())
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(mapped.clone())
    }

    pub(crate) fn resolve(&self, parts: &Parts) -> Result<UserId, ApiError> {
        if let Some(header) = parts.headers.get(axum::http::header::AUTHORIZATION) {
            let text = header.to_str().map_err(|_| ApiError::Unauthorized)?;
            let token = text.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
            let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            let mut found = None;
            for (secret, user) in &self.tokens {
                if bool::from(secret.ct_eq(&digest)) {
                    found = Some(user.clone());
                }
            }
            return found.ok_or(ApiError::Unauthorized);
        }
        if let Some(peer) = parts.extensions.get::<PeerCertificate>() {
            return self.device_user(peer);
        }
        if let Some(ConnectInfo(remote)) = parts.extensions.get::<ConnectInfo<SocketAddr>>()
            && crate::auth::is_loopback_remote(*remote)
        {
            return self.local.clone().ok_or(ApiError::Unauthorized);
        }
        Err(ApiError::Unauthorized)
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
    if gateway.client_certificate_required()
        || parts
            .headers
            .contains_key(axum::http::header::AUTHORIZATION)
    {
        return gateway.tenancy.resolve(parts);
    }
    match gateway.tenancy.resolve(parts) {
        Ok(user) => Ok(user),
        Err(error) => anonymous_owner(&gateway.tenancy, error),
    }
}

fn anonymous_owner(tenancy: &Tenancy, error: ApiError) -> Result<UserId, ApiError> {
    match tenancy.local_user() {
        Some(user) => Ok(user.clone()),
        None => Err(error),
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

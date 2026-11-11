use std::collections::BTreeSet;
use std::fmt;

use config::Config;
use thiserror::Error;

use crate::seal::{CodeError, ShareServer};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("memory.share.allowed_servers: {0}")]
pub struct BadAllowedServer(#[from] CodeError);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ShareRefused {
    #[error(
        "sharing is disabled by this hub's policy: cloud.disabled is set and memory.share.allowed_servers names no share server"
    )]
    Off,
    #[error(
        "sharing through rivethub.cloud is disabled by this hub's policy (cloud.disabled); set memory.share.server.url to a server in memory.share.allowed_servers"
    )]
    Cloud,
    #[error(
        "sharing with {host} is disabled by this hub's policy; memory.share.allowed_servers allows only {allowed}"
    )]
    Unlisted { host: ShareServer, allowed: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePolicy {
    cloud_disabled: bool,
    allowed: Option<BTreeSet<ShareServer>>,
}

impl SharePolicy {
    pub fn from_config(config: &Config) -> Result<Self, BadAllowedServer> {
        Self::new(config.cloud_disabled(), config.share_allowed_servers())
    }

    pub fn new(cloud_disabled: bool, allowed_servers: &[String]) -> Result<Self, BadAllowedServer> {
        let listed = allowed_servers
            .iter()
            .map(|entry| entry.parse())
            .collect::<Result<BTreeSet<ShareServer>, CodeError>>()?;
        Ok(Self {
            cloud_disabled,
            allowed: (!listed.is_empty()).then_some(listed),
        })
    }

    pub fn admit(&self, host: &ShareServer) -> Result<(), ShareRefused> {
        match (self.cloud_disabled, &self.allowed) {
            (true, None) => Err(ShareRefused::Off),
            (true, Some(_)) if host.is_default() => Err(ShareRefused::Cloud),
            (_, Some(allowed)) if !allowed.contains(host) => Err(ShareRefused::Unlisted {
                host: host.clone(),
                allowed: joined(allowed),
            }),
            _ => Ok(()),
        }
    }

    pub fn default_server(
        &self,
        configured: Option<ShareServer>,
    ) -> Result<ShareServer, ShareRefused> {
        let host = configured.unwrap_or_else(ShareServer::rivethub_cloud);
        self.admit(&host)?;
        Ok(host)
    }
}

impl fmt::Display for SharePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(allowed) = &self.allowed else {
            return formatter.write_str(if self.cloud_disabled {
                "no share server"
            } else {
                "any share server"
            });
        };
        let reachable: BTreeSet<_> = allowed
            .iter()
            .filter(|host| self.admit(host).is_ok())
            .cloned()
            .collect();
        if reachable.is_empty() {
            formatter.write_str("no share server")
        } else {
            write!(formatter, "only {}", joined(&reachable))
        }
    }
}

fn joined(hosts: &BTreeSet<ShareServer>) -> String {
    hosts
        .iter()
        .map(ShareServer::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

use std::fmt;

use capture::seal::ShareServer;
use reqwest::StatusCode;
use wire::gateway::tenancy::MeAccess;
use wire::gateway::tenancy::MeResponse;
use wire::gateway::users::TokenScope;
use wire::gateway::users::TokenScopes;

use super::ME_PATH;
use super::Server;
use super::ServerError;
use super::ShareServers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hub {
    pub(crate) name: Option<String>,
    pub(crate) server: ShareServer,
}

impl fmt::Display for Hub {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(formatter, "{name} ({})", self.server),
            None => write!(formatter, "{}", self.server),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub(crate) user: String,
    pub(crate) access: Option<MeAccess>,
}

impl Member {
    pub(crate) fn may(&self, scope: TokenScope) -> Option<bool> {
        let access = self.access.as_ref()?;
        let known: Vec<TokenScope> = access
            .scopes
            .iter()
            .filter_map(|name| TokenScope::named(name))
            .collect();
        Some(TokenScopes::of(&known).is_some_and(|scopes| scopes.allows(scope)))
    }
}

impl ShareServers {
    pub(crate) fn hub(&self, named: &str) -> Result<Hub, ServerError> {
        let wanted = named.trim();
        let Some(entry) = self.hubs.iter().find(|entry| entry.name == wanted) else {
            let server: ShareServer = wanted
                .parse()
                .map_err(|_| ServerError::UnknownHub(wanted.to_owned()))?;
            return Ok(Hub {
                name: self.hub_named(server.as_str()),
                server,
            });
        };
        let server: ShareServer = entry
            .url
            .as_str()
            .parse()
            .map_err(|_| ServerError::HubUrl {
                name: entry.name.clone(),
                url: entry.url.to_string(),
            })?;
        match wanted.parse::<ShareServer>() {
            Ok(host) if host != server && self.reaches(&host)? => Err(ServerError::AmbiguousHub {
                name: entry.name.clone(),
                named: server,
                host,
            }),
            _ => Ok(Hub {
                name: Some(entry.name.clone()),
                server,
            }),
        }
    }

    fn reaches(&self, host: &ShareServer) -> Result<bool, ServerError> {
        Ok(self.configured.as_ref() == Some(host)
            || self.hub_named(host.as_str()).is_some()
            || self.logins.list()?.contains_key(host.as_str()))
    }

    pub(crate) fn hub_named(&self, host: &str) -> Option<String> {
        self.hubs
            .iter()
            .find(|entry| {
                entry
                    .url
                    .as_str()
                    .parse::<ShareServer>()
                    .is_ok_and(|parsed| parsed.as_str() == host)
            })
            .map(|entry| entry.name.clone())
    }

    pub(crate) fn hubs(&self) -> Vec<Hub> {
        self.hubs
            .iter()
            .filter_map(|entry| {
                #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
                let Ok(server) = entry.url.as_str().parse() else {
                    return None;
                };
                Some(Hub {
                    name: Some(entry.name.clone()),
                    server,
                })
            })
            .collect()
    }
}

impl Server {
    pub(crate) async fn member(&self) -> Result<Member, ServerError> {
        let url = self.endpoint(ME_PATH)?;
        let mut rejected: Option<String> = None;
        loop {
            let token = self.access_token(rejected.as_deref()).await?;
            let response = self
                .client()
                .get(url.clone())
                .bearer_auth(&token)
                .send()
                .await?;
            match response.status() {
                StatusCode::UNAUTHORIZED if rejected.is_none() => {
                    rejected = Some(token);
                }
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(ServerError::TokenRefused {
                        server: self.host().clone(),
                        status: response.status().as_u16(),
                    });
                }
                _ => {
                    let me: MeResponse = super::checked(response)?.json().await?;
                    return Ok(Member {
                        user: me.user,
                        access: me.access,
                    });
                }
            }
        }
    }

    pub(crate) fn refused(&self, status: StatusCode, scope: TokenScope) -> ServerError {
        match status {
            StatusCode::UNAUTHORIZED => ServerError::TokenRefused {
                server: self.host().clone(),
                status: status.as_u16(),
            },
            StatusCode::FORBIDDEN => ServerError::MissingScope {
                server: self.host().clone(),
                scope,
            },
            other => ServerError::Answered(other.as_u16()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Member;
    use wire::gateway::tenancy::MeAccess;
    use wire::gateway::users::TokenScope;

    #[test]
    fn a_scope_this_version_does_not_know_is_ignored() {
        let member = Member {
            user: "ana".to_owned(),
            access: Some(MeAccess {
                scopes: vec!["memory:import".to_owned(), "wiki:write".to_owned()],
                drops_enabled: false,
            }),
        };
        assert_eq!(member.may(TokenScope::MemoryImport), Some(true));
        assert_eq!(member.may(TokenScope::DropsWrite), Some(false));
    }
}

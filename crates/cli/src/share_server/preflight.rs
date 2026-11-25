use std::time::Duration;

use wire::gateway::users::TokenScope;

use super::Login;
use super::ServerError;
use super::ShareServers;
use super::hub::Hub;
use super::hub::Member;
use crate::Exit;
use crate::Report;

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_PATH: &str = "healthz";

impl ShareServers {
    pub(crate) async fn preflight(&self, named: &str) -> Result<Report, ServerError> {
        let hub = self.hub(named)?;
        let mut lines = vec![hub.to_string()];
        let server = match self.connect(&hub.server, CHECK_TIMEOUT).await {
            Ok(server) => server,
            Err(ServerError::Policy(refused)) => return Ok(failed(lines, &refused)),
            Err(error) => return Err(error),
        };
        let health = server
            .client()
            .get(server.endpoint(HEALTH_PATH)?)
            .send()
            .await;
        match health {
            Ok(answer) if answer.status().is_success() => {
                let scheme = if hub.server.is_loopback() {
                    "plain HTTP on loopback"
                } else {
                    "TLS"
                };
                lines.push(format!("reachable over {scheme}"));
            }
            Ok(answer) => {
                return Ok(failed(
                    lines,
                    &format!(
                        "answered HTTP {} at /healthz, so it is not a RivetHub hub rh can use",
                        answer.status().as_u16()
                    ),
                ));
            }
            Err(error) => return Ok(failed(lines, &unreachable(&error))),
        }
        let how = match self.logins.load(&hub.server).await {
            Ok(Login::Team(_)) => "a team token",
            Ok(Login::Device(_)) => "a browser login",
            Err(ServerError::LoggedOut(_)) => {
                return Ok(failed(
                    lines,
                    &format!(
                        "not signed in; run rh share login {} --token-file <file> with the token the hub admin gave you",
                        hub.server
                    ),
                ));
            }
            Err(error) => return Err(error),
        };
        let member = match server.member().await {
            Ok(member) => member,
            Err(error @ ServerError::TokenRefused { .. }) => return Ok(failed(lines, &error)),
            Err(error) => return Err(error),
        };
        lines.push(format!("signed in as {} with {how}", member.user));
        lines.extend(capabilities(&hub, &member));
        Ok(Report::success(joined(&lines)))
    }

    pub(crate) fn hub_lines(&self) -> String {
        self.hubs().iter().fold(String::new(), |mut lines, hub| {
            lines.push_str("hub ");
            lines.push_str(&hub.to_string());
            lines.push_str("; rh share status --server ");
            lines.push_str(hub.name.as_deref().unwrap_or(hub.server.as_str()));
            lines.push_str(" checks it\n");
            lines
        })
    }
}

fn capabilities(hub: &Hub, member: &Member) -> Vec<String> {
    let Some(access) = &member.access else {
        return vec![
            "token scopes: unknown, because the hub is older than this rh; rh memory share says what it refuses"
                .to_owned(),
        ];
    };
    let scopes = access.scopes.clone();
    let host = &hub.server;
    let target = hub.name.as_deref().unwrap_or(host.as_str());
    let drops = match (
        access.drops_enabled,
        member.may(TokenScope::DropsWrite) == Some(true),
    ) {
        (false, _) => "drops: off; this server has drops turned off".to_owned(),
        (true, false) => {
            "drops: on, but your token lacks drops:write; ask the admin for a drops:write token"
                .to_owned()
        }
        (true, true) => {
            format!("drops: on; rh memory share <bundle> --server {host} gives a share code")
        }
    };
    let import = if member.may(TokenScope::MemoryImport) == Some(true) {
        format!(
            "memory import: allowed; rh memory share <bundle> --hub {target} pushes sessions into your memory on it"
        )
    } else {
        "memory import: not allowed; your token lacks memory:import, so ask the admin for a memory:import token"
            .to_owned()
    };
    vec![
        format!("token scopes: {}", scopes.join(", ")),
        drops,
        import,
    ]
}

fn joined(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn unreachable(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    format!("unreachable: {text}")
}

fn failed(mut lines: Vec<String>, reason: &impl std::fmt::Display) -> Report {
    lines.push(reason.to_string());
    Report {
        stdout: joined(&lines),
        stderr: String::new(),
        exit: Exit::Failure,
        stdout_raw: Vec::new(),
    }
}

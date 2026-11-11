use std::collections::BTreeMap;
use std::str::FromStr;

use serde::Deserialize;
use values::{HarnessId, SessionId};

#[derive(strum::EnumString)]
#[strum(serialize_all = "kebab-case")]
enum CredentialName {
    Authorization,
    ProxyAuthorization,
    XApiKey,
    Cookie,
    ChatgptAccountId,
}

struct Correlation {
    harness: HarnessId,
    session_header: &'static str,
    user_agent_prefix: &'static str,
    turn_header: Option<&'static str>,
}

const CORRELATIONS: [Correlation; 2] = [
    Correlation {
        harness: HarnessId::ClaudeCode,
        session_header: "x-claude-code-session-id",
        user_agent_prefix: "claude-cli/",
        turn_header: None,
    },
    Correlation {
        harness: HarnessId::Codex,
        session_header: "session-id",
        user_agent_prefix: "codex_",
        turn_header: Some("x-codex-turn-metadata"),
    },
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("credential header {name} is refused")]
pub struct CredentialHeader {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyHeaders(BTreeMap<String, String>);

impl ProxyHeaders {
    pub fn parse(
        headers: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, CredentialHeader> {
        let mut lowered = BTreeMap::new();
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            if credential(&name) {
                return Err(CredentialHeader { name });
            }
            lowered.insert(name, value);
        }
        Ok(Self(lowered))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

fn credential(name: &str) -> bool {
    CredentialName::from_str(name).is_ok() || name.contains("token") || name.contains("secret")
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct TurnMetadata {
    pub turn_id: Option<String>,
    pub request_kind: Option<String>,
}

impl TurnMetadata {
    #[must_use]
    pub fn of(turn_id: Option<String>, request_kind: Option<String>) -> Option<Self> {
        (turn_id.is_some() || request_kind.is_some()).then_some(Self {
            turn_id,
            request_kind,
        })
    }

    fn parse(raw: &str) -> Option<Self> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B3: a malformed x-codex-turn-metadata header is ignored and the call keeps its session (note 125)"
            )
        )]
        let parsed = serde_json::from_str::<Self>(raw).ok()?;
        Self::of(parsed.turn_id, parsed.request_kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correlated {
    pub session: SessionId,
    pub turn: Option<TurnMetadata>,
}

#[must_use]
pub fn correlate(headers: &ProxyHeaders) -> Option<Correlated> {
    let user_agent = headers.get("user-agent")?;
    CORRELATIONS.iter().find_map(|row| {
        if !user_agent.starts_with(row.user_agent_prefix) {
            return None;
        }
        let native = headers.get(row.session_header)?.trim();
        if native.is_empty() || native.chars().any(char::is_control) {
            return None;
        }
        let turn = row
            .turn_header
            .and_then(|name| headers.get(name))
            .and_then(TurnMetadata::parse);
        Some(Correlated {
            session: SessionId::new(row.harness, native),
            turn,
        })
    })
}

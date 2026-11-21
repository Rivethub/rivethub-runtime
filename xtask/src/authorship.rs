use std::path::Path;
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::Result;
use crate::git::git;

#[derive(Debug, Clone, Copy)]
enum AiVendor {
    Aider,
    Amp,
    Anthropic,
    Codeium,
    Cognition,
    Cursor,
    Factory,
    GitHubCopilot,
    Google,
    Kiro,
    OpenAi,
    OpenCode,
    OpenHands,
    Warp,
    Xai,
}

impl AiVendor {
    const ALL: [Self; 15] = [
        Self::Aider,
        Self::Amp,
        Self::Anthropic,
        Self::Codeium,
        Self::Cognition,
        Self::Cursor,
        Self::Factory,
        Self::GitHubCopilot,
        Self::Google,
        Self::Kiro,
        Self::OpenAi,
        Self::OpenCode,
        Self::OpenHands,
        Self::Warp,
        Self::Xai,
    ];

    const fn domains(self) -> &'static [&'static str] {
        match self {
            Self::Aider => &["aider.chat"],
            Self::Amp => &["ampcode.com"],
            Self::Anthropic => &["anthropic.com", "claude.ai", "claude.com"],
            Self::Codeium => &["codeium.com", "windsurf.com"],
            Self::Cognition => &["cognition.ai", "devin.ai"],
            Self::Cursor => &["cursor.com", "cursor.sh"],
            Self::Factory => &["factory.ai"],
            Self::GitHubCopilot | Self::Google => &[],
            Self::Kiro => &["kiro.dev"],
            Self::OpenAi => &["openai.com", "chatgpt.com"],
            Self::OpenCode => &["opencode.ai"],
            Self::OpenHands => &["all-hands.dev"],
            Self::Warp => &["warp.dev"],
            Self::Xai => &["x.ai"],
        }
    }

    const fn github_logins(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &["claude[bot]"],
            Self::Cognition => &["devin-ai-integration[bot]"],
            Self::Cursor => &["cursor[bot]", "cursoragent"],
            Self::GitHubCopilot => &["copilot", "copilot-swe-agent[bot]"],
            Self::Google => &["gemini-code-assist[bot]", "google-labs-jules[bot]"],
            Self::OpenAi => &["chatgpt-codex-connector[bot]", "openai-codex[bot]"],
            Self::Factory => &["factory-droid[bot]"],
            Self::Aider
            | Self::Amp
            | Self::Codeium
            | Self::Kiro
            | Self::OpenCode
            | Self::OpenHands
            | Self::Warp
            | Self::Xai => &[],
        }
    }

    fn owns(self, local: &str, domain: &str) -> bool {
        let login = local.rsplit_once('+').map_or(local, |(_, login)| login);
        self.domains()
            .iter()
            .any(|vendor| domain == *vendor || domain.ends_with(&format!(".{vendor}")))
            || (domain == "users.noreply.github.com" && self.github_logins().contains(&login))
    }
}

#[expect(clippy::expect_used)]
static AI_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(claude (code|opus|sonnet|haiku|fable|mythos|\d)\b.*|cursor( agent)?( v?[\d.]+)?|(github )?copilot\b.*|(openai )?codex\b.*|chatgpt\b.*|gemini( (cli|code assist|pro|flash|ultra|nano|\d)\b.*)?|devin ai\b.*|grok( .*)?|aider\b.*|.*\(aider\)|opencode( .*)?|openhands|windsurf|antigravity|warp|factory droid)$",
    )
    .expect("the AI-name pattern is a constant")
});
#[expect(clippy::expect_used)]
static TRAILER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z-]*):\s*(.*)$").expect("the trailer pattern is a constant")
});
#[expect(clippy::expect_used)]
static CREDITED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^<]*?)\s*(?:<([^>]*)>.*)?$").expect("the credited pattern is a constant")
});
#[expect(clippy::expect_used)]
static FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[^\x00-\x7F]+\s*)?Generated (?:with|by) (.+)$")
        .expect("the footer pattern is a constant")
});
#[expect(clippy::expect_used)]
static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[([^\]]*)\]\(([^)]*)\)$").expect("the link pattern is a constant")
});
#[expect(clippy::expect_used)]
static URL_HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)https?://([^/\s:)>\]]+)").expect("the URL pattern is a constant")
});

const SCISSORS: &str = "# ------------------------ >8 ------------------------";
const FOOTER_WORDS: usize = 4;

#[derive(Debug, Clone, Copy, strum::EnumString)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
enum TrailerKey {
    CoAuthoredBy,
    SignedOffBy,
    AssistedBy,
    HelpedBy,
    MadeWith,
    MadeBy,
    GeneratedWith,
    GeneratedBy,
    WrittenBy,
    AuthoredBy,
    BuiltWith,
    CreatedWith,
    CreatedBy,
    ClaudeSession,
    AmpThreadId,
}

impl TrailerKey {
    const fn names_a_session(self) -> bool {
        match self {
            Self::ClaudeSession | Self::AmpThreadId => true,
            Self::CoAuthoredBy
            | Self::SignedOffBy
            | Self::AssistedBy
            | Self::HelpedBy
            | Self::MadeWith
            | Self::MadeBy
            | Self::GeneratedWith
            | Self::GeneratedBy
            | Self::WrittenBy
            | Self::AuthoredBy
            | Self::BuiltWith
            | Self::CreatedWith
            | Self::CreatedBy => false,
        }
    }
}

#[must_use]
pub fn ai_product(name: &str, email: &str) -> bool {
    let email = email.trim().to_lowercase();
    let (local, domain) = email.rsplit_once('@').unwrap_or(("", email.as_str()));
    AiVendor::ALL
        .iter()
        .any(|vendor| vendor.owns(local, domain))
        || AI_NAME.is_match(name.trim())
}

fn vendor_host(host: &str) -> bool {
    let host = host.to_lowercase();
    AiVendor::ALL.iter().any(|vendor| vendor.owns("", &host))
}

fn credits_ai(value: &str) -> bool {
    let value = value.trim().trim_end_matches(['.', ',', ';']);
    let linked = URL_HOST
        .captures_iter(value)
        .filter_map(|url| url.get(1))
        .any(|host| vendor_host(host.as_str()));
    linked
        || CREDITED.captures(value).is_some_and(|credited| {
            let name = credited.get(1).map_or("", |name| name.as_str());
            match credited.get(2) {
                Some(email) => ai_product(name, email.as_str()),
                None if name.contains('@') => ai_product("", name),
                None => ai_product(name, ""),
            }
        })
}

fn ai_trailer(line: &str) -> Option<String> {
    let trailer = TRAILER.captures(line)?;
    let name = trailer.get(1)?.as_str();
    let value = trailer.get(2)?.as_str().trim();
    match TrailerKey::from_str(name) {
        Ok(key) if key.names_a_session() => Some(format!("a {name} trailer")),
        Ok(_) if credits_ai(value) => Some(format!("{name}: {value}")),
        Ok(_) | Err(strum::ParseError::VariantNotFound) => None,
    }
}

fn footer_credits_ai(credit: &str) -> bool {
    match MARKDOWN_LINK.captures(credit.trim()) {
        Some(link) => [link.get(1), link.get(2)]
            .into_iter()
            .flatten()
            .any(|part| credits_ai(part.as_str())),
        None => credit.split_whitespace().count() <= FOOTER_WORDS && credits_ai(credit),
    }
}

fn ai_footer(text: &str) -> Option<String> {
    let closing = text.trim_end().rsplit("\n\n").find(|paragraph| {
        paragraph
            .lines()
            .any(|line| !line.trim().is_empty() && !TRAILER.is_match(line))
    })?;
    closing.lines().map(str::trim_end).find_map(|line| {
        let credit = FOOTER.captures(line)?.get(1)?.as_str();
        footer_credits_ai(credit).then(|| line.to_owned())
    })
}

#[must_use]
pub fn edited_message(raw: &str) -> String {
    raw.lines()
        .take_while(|line| *line != SCISSORS)
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn commit_message(findings: &mut Vec<String>, text: &str, author: Option<(&str, &str)>) {
    let text = text.replace("\r\n", "\n");
    let credits = text
        .lines()
        .filter_map(|line| ai_trailer(line.trim_end()))
        .chain(ai_footer(&text));
    for credit in credits {
        findings.push(format!(
            "AUTHORSHIP commit message credits an AI product ({credit})"
        ));
    }
    if let Some((name, email)) = author {
        person(findings, name, email, "authors the commit");
    }
}

pub fn person(findings: &mut Vec<String>, name: &str, email: &str, does: &str) {
    if ai_product(name, email) {
        findings.push(format!(
            "AUTHORSHIP {name} <{email}> is an AI product; the person responsible {does}"
        ));
    }
}

pub fn commits(root: &Path, range: &str, findings: &mut Vec<String>) -> Result<usize> {
    let out = git(
        root,
        &[
            "log",
            "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%B%x1e",
            range,
        ],
    )?;
    let mut count = 0;
    for record in out.split('\u{1e}') {
        let record = record.trim_matches('\n');
        if record.is_empty() {
            continue;
        }
        let mut parts = record.split('\u{1f}');
        let (Some(hash), Some(name), Some(email), Some(committer), Some(committed_as), Some(body)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        count += 1;
        let before = findings.len();
        commit_message(findings, body, Some((name, email)));
        if (committer, committed_as) != (name, email) {
            person(findings, committer, committed_as, "commits");
        }
        let suffix = hash.get(..8).unwrap_or(hash).to_owned();
        for finding in findings.iter_mut().skip(before) {
            finding.push_str(" (");
            finding.push_str(&suffix);
            finding.push(')');
        }
    }
    Ok(count)
}

use std::path::Path;
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
    Xai,
}

impl AiVendor {
    const ALL: [Self; 13] = [
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
            Self::Aider | Self::Amp | Self::Codeium | Self::Kiro | Self::OpenCode | Self::Xai => {
                &[]
            }
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
        r"(?i)^(claude (code|opus|sonnet|haiku|fable|mythos)\b.*|cursor( agent)?|(github )?copilot\b.*|(openai )?codex\b.*|chatgpt\b.*|gemini( (cli|code assist|pro|flash|ultra|nano|\d).*)?|devin ai\b.*|grok( .*)?|aider\b.*|.*\(aider\)|opencode|windsurf|antigravity|factory droid)$",
    )
    .expect("the AI-name pattern is a constant")
});
#[expect(clippy::expect_used)]
static CO_AUTHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^co-authored-by:\s*([^<]*?)\s*(?:<([^>]*)>)?$")
        .expect("the co-author pattern is a constant")
});
#[expect(clippy::expect_used)]
static AI_TRAILER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(Claude|Cursor|Codex|Copilot|Amp|Devin|Gemini|Aider|OpenCode|Windsurf|Grok|Kiro|Droid)-[A-Za-z-]+: ",
    )
    .expect("the AI-trailer pattern is a constant")
});
#[expect(clippy::expect_used)]
static CREDIT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\W*(generated|made|created|written|authored|built)[ -](with|by|using)\b")
        .expect("the credit-line pattern is a constant")
});
#[expect(clippy::expect_used)]
static AI_PRODUCT_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(Claude|Anthropic|Cursor|Copilot|ChatGPT|OpenAI|Codex|Gemini|Devin|Aider|aider|Windsurf|OpenCode|opencode|Amp|Grok|Jules|Kiro|Droid)\b",
    )
    .expect("the AI-product pattern is a constant")
});
#[expect(clippy::expect_used)]
static AI_SESSION_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)https?://[^\s/]*(claude\.ai|claude\.com|devin\.ai|ampcode\.com|chatgpt\.com|cursor\.com)/\S*(session|thread|share)",
    )
    .expect("the AI-session pattern is a constant")
});

fn ai_credit(line: &str) -> bool {
    AI_TRAILER.is_match(line)
        || AI_SESSION_URL.is_match(line)
        || (CREDIT_LINE.is_match(line) && AI_PRODUCT_WORD.is_match(line))
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

pub fn commit_message(findings: &mut Vec<String>, text: &str, author: Option<(&str, &str)>) {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    for credit in lines.iter().filter_map(|line| CO_AUTHOR.captures(line)) {
        let name = credit.get(1).map_or("", |name| name.as_str());
        let email = credit.get(2).map_or("", |email| email.as_str());
        if ai_product(name, email) {
            findings.push(format!(
                "AUTHORSHIP commit message credits an AI product as co-author ({name})"
            ));
        }
    }
    if lines.iter().any(|line| ai_credit(line)) {
        findings.push("AUTHORSHIP commit message carries AI attribution text".to_owned());
    }
    if let Some((name, email)) = author
        && ai_product(name, email)
    {
        findings.push(format!(
            "AUTHORSHIP {name} <{email}> is an AI product; the person responsible authors the commit"
        ));
    }
}

pub fn commits(root: &Path, range: &str, findings: &mut Vec<String>) -> Result<usize> {
    let out = git(root, &["log", "--format=%H%x1f%an%x1f%ae%x1f%B%x1e", range])?;
    let mut count = 0;
    for record in out.split('\u{1e}') {
        let record = record.trim_matches('\n');
        if record.is_empty() {
            continue;
        }
        let mut parts = record.split('\u{1f}');
        let (Some(hash), Some(name), Some(email), Some(body)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        count += 1;
        let before = findings.len();
        commit_message(findings, body, Some((name, email)));
        let suffix = hash.get(..8).unwrap_or(hash).to_owned();
        for finding in findings.iter_mut().skip(before) {
            finding.push_str(" (");
            finding.push_str(&suffix);
            finding.push(')');
        }
    }
    Ok(count)
}

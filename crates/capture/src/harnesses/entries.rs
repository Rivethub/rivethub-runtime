use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum CommandField {
    Line(String),
    Argv(Vec<String>),
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct McpServer {
    command: Option<CommandField>,
    #[serde(default)]
    args: Vec<String>,
    url: Option<String>,
}

impl McpServer {
    pub(super) fn invocation(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        match &self.command {
            Some(CommandField::Line(line)) => parts.push(line),
            Some(CommandField::Argv(argv)) => parts.extend(argv.iter().map(String::as_str)),
            None => {}
        }
        parts.extend(self.args.iter().map(String::as_str));
        parts.extend(self.url.as_deref());
        parts.join(" ")
    }

    pub(super) fn runs_rh_mcp(&self) -> bool {
        runs_rh_mcp(&self.invocation())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct JsonServers {
    #[serde(default, rename = "mcpServers")]
    pub(super) standard: BTreeMap<String, McpServer>,
    #[serde(default)]
    pub(super) mcp: BTreeMap<String, McpServer>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct TomlServers {
    #[serde(default)]
    pub(super) mcp_servers: BTreeMap<String, McpServer>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct HookFile {
    #[serde(default)]
    pub(super) hooks: BTreeMap<String, Vec<HookGroup>>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct HookGroup {
    #[serde(default)]
    pub(super) hooks: Vec<HookCommand>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct HookCommand {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

impl HookCommand {
    pub(super) fn invocation(&self) -> String {
        let mut parts: Vec<&str> = self.command.as_deref().into_iter().collect();
        parts.extend(self.args.iter().map(String::as_str));
        parts.join(" ")
    }
}

fn words(text: &str) -> Vec<String> {
    text.replace(['"', '\'', '\\'], "")
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_rh(word: &str) -> bool {
    Path::new(word).file_name().is_some_and(|name| name == "rh")
}

pub(super) fn runs_rh_mcp(text: &str) -> bool {
    words(text)
        .windows(2)
        .any(|pair| matches!(pair, [binary, sub] if is_rh(binary) && sub == "mcp"))
}

pub(super) fn capture_hook_harness(text: &str) -> Option<String> {
    let words = words(text);
    let at = words.windows(3).position(
        |triple| matches!(triple, [binary, capture, hook] if is_rh(binary) && capture == "capture" && hook == "hook"),
    )?;
    let rest = words.get(at + 3..).unwrap_or(&[]);
    let named = rest
        .windows(2)
        .find(|pair| matches!(pair, [flag, _] if flag == "--harness"))
        .and_then(|pair| pair.get(1).cloned());
    Some(named.unwrap_or_else(|| "claude-code".to_owned()))
}

impl HookGroup {
    pub(super) fn runs_capture(&self, harness: &str) -> bool {
        self.hooks.iter().any(|hook| {
            capture_hook_harness(&hook.invocation()).is_some_and(|named| named == harness)
        })
    }

    pub(super) fn is_rh_capture(&self, harness: &str) -> bool {
        !self.hooks.is_empty()
            && self.hooks.iter().all(|hook| {
                capture_hook_harness(&hook.invocation()).is_some_and(|named| named == harness)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bash_wrapper_that_execs_rh_mcp_counts() {
        assert!(runs_rh_mcp(
            "bash -c set -a; . \"$HOME/.rivethub/.env\"; set +a; exec \"$HOME/.local/bin/rh\" mcp"
        ));
        assert!(!runs_rh_mcp("node server.js mcp"));
    }

    #[test]
    fn the_hook_harness_defaults_to_claude_code() {
        assert_eq!(
            capture_hook_harness("/home/u/.local/bin/rh capture hook").as_deref(),
            Some("claude-code")
        );
        assert_eq!(
            capture_hook_harness("/x/rh capture hook --harness codex").as_deref(),
            Some("codex")
        );
        assert_eq!(capture_hook_harness("other capture hook"), None);
    }
}

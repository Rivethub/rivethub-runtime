use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

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
    pub(super) fn from_command(command: &str, args: &[String]) -> Self {
        Self {
            command: Some(CommandField::Line(command.to_owned())),
            args: args.to_vec(),
            url: None,
        }
    }

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
        let mut argv: Vec<&str> = match &self.command {
            Some(CommandField::Line(line)) => vec![line.as_str()],
            Some(CommandField::Argv(argv)) => argv.iter().map(String::as_str).collect(),
            None => return false,
        };
        argv.extend(self.args.iter().map(String::as_str));
        match argv.as_slice() {
            [rh, "mcp", ..] => is_rh(rh),
            [bash, "-c", script] => is_named(bash, "bash") && wrapper_runs_rh_mcp(script),
            _ => false,
        }
    }
}

const SHELL_WORD: &str = r#"(?:'[^']*'|"(?:[^"\\]|\\.)*"|\\.|[^\s'";&|<>()`\\#])(?:'[^']*'|"(?:[^"\\]|\\.)*"|\\.|[^\s'";&|<>()`\\])*"#;

#[expect(clippy::expect_used)]
static WRAPPER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^set -a; \. {SHELL_WORD}(?: >/dev/null 2>&1)?; set \+a; exec ({SHELL_WORD}) mcp$"
    ))
    .expect("the wrapper pattern compiles")
});

fn wrapper_runs_rh_mcp(script: &str) -> bool {
    WRAPPER
        .captures(script)
        .and_then(|captures| captures.get(1))
        .and_then(|rh| shlex::split(rh.as_str()))
        .is_some_and(|words| matches!(words.as_slice(), [rh] if is_rh(rh)))
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
pub(super) struct YamlServers {
    #[serde(default)]
    pub(super) mcp_servers: Option<BTreeMap<String, McpServer>>,
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

fn is_named(word: &str, binary: &str) -> bool {
    Path::new(word)
        .file_name()
        .is_some_and(|name| name == binary)
}

fn is_rh(word: &str) -> bool {
    is_named(word, "rh")
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
    fn only_the_exact_generated_wrappers_count_as_running_rh_mcp() {
        let server = |script: &str| McpServer {
            command: Some(CommandField::Line("bash".to_owned())),
            args: vec!["-c".to_owned(), script.to_owned()],
            url: None,
        };
        assert!(
            server("set -a; . \"$HOME/.rivethub/.env\"; set +a; exec \"$HOME/.local/bin/rh\" mcp")
                .runs_rh_mcp()
        );
        assert!(
            server("set -a; . /h/.env >/dev/null 2>&1; set +a; exec '/h/it'\"'\"'s/rh' mcp")
                .runs_rh_mcp()
        );
        assert!(
            !server("set -a; echo \"set\" \"+a;\" \"exec\" \"/usr/bin/rh\" \"mcp\"").runs_rh_mcp()
        );
        assert!(!server("set -a; . /h/.env; set +a; exec /usr/bin/other mcp").runs_rh_mcp());
        assert!(!server("set -a; . #; set +a; exec /usr/bin/rh mcp").runs_rh_mcp());
        assert!(!server("set -a; . /h/.env; set +a; exec #/usr/bin/rh mcp").runs_rh_mcp());
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

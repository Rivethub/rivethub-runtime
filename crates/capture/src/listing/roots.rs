use std::path::Path;
use std::path::PathBuf;

use super::StoreRoots;
use config::Environment;

fn setting<'a>(env: &'a Environment, name: &str) -> Option<&'a str> {
    env.text(name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn home(env: &Environment) -> Option<&Path> {
    env.home().filter(|home| !home.is_empty()).map(Path::new)
}

fn home_dir(env: &Environment, relative: &str) -> Option<PathBuf> {
    home(env).map(|home| home.join(relative))
}

fn overridable(env: &Environment, name: &str, relative: &str) -> Option<PathBuf> {
    setting(env, name)
        .map(PathBuf::from)
        .or_else(|| home_dir(env, relative))
}

fn cowork_roots(env: &Environment) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let candidates = [
        home_dir(env, "Library/Application Support/Claude"),
        home_dir(env, ".config/Claude"),
        setting(env, "CLAUDE_CONFIG_DIR").map(PathBuf::from),
    ];
    for root in candidates.into_iter().flatten() {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn antigravity_roots(env: &Environment) -> Vec<PathBuf> {
    if let Some(explicit) = setting(env, "ANTIGRAVITY_HOME") {
        return vec![PathBuf::from(explicit)];
    }
    let mut roots = Vec::new();
    for relative in [
        ".gemini/antigravity-cli",
        ".gemini/antigravity",
        ".gemini/antigravity-ide",
    ] {
        if let Some(root) = home_dir(env, relative)
            && !roots.contains(&root)
        {
            roots.push(root);
        }
    }
    roots
}

#[must_use]
pub fn store_roots(env: &Environment) -> StoreRoots {
    StoreRoots {
        claude: overridable(env, "CLAUDE_CONFIG_DIR", ".claude"),
        grok: overridable(env, "GROK_HOME", ".grok"),
        hermes: overridable(env, "HERMES_HOME", ".hermes"),
        kimi: overridable(env, "KIMI_CODE_HOME", ".kimi-code"),
        codex: overridable(env, "CODEX_HOME", ".codex"),
        opencode: overridable(env, "XDG_DATA_HOME", ".local/share")
            .map(|data| data.join("opencode")),
        omp: overridable(env, "PI_CODING_AGENT_DIR", ".omp/agent"),
        cursor: home_dir(env, ".cursor"),
        cowork: cowork_roots(env),
        dsh: overridable(env, "DSH_HOME", ".dsh"),
        antigravity: antigravity_roots(env),
    }
}

#[must_use]
pub fn claude_config_file(env: &Environment) -> Option<PathBuf> {
    setting(env, "CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home(env).map(Path::to_path_buf))
        .map(|directory| directory.join(".claude.json"))
}

use std::path::PathBuf;

use capture::listing::StoreRoots;
use config::Environment;
use rivethub::composition::store_roots;

fn roots(vars: &[(&str, &str)]) -> StoreRoots {
    store_roots(&Environment::from_vars(vars.iter().copied()))
}

#[test]
fn store_roots_default_under_home() {
    let roots = roots(&[("HOME", "/home/user")]);
    let home = PathBuf::from("/home/user");
    assert_eq!(
        roots,
        StoreRoots {
            claude: Some(home.join(".claude")),
            grok: Some(home.join(".grok")),
            hermes: Some(home.join(".hermes")),
            kimi: Some(home.join(".kimi-code")),
            codex: Some(home.join(".codex")),
            opencode: Some(home.join(".local/share/opencode")),
            omp: Some(home.join(".omp/agent")),
            cursor: Some(home.join(".cursor")),
            cowork: vec![
                home.join("Library/Application Support/Claude"),
                home.join(".config/Claude"),
            ],
            dsh: Some(home.join(".dsh")),
            antigravity: vec![
                home.join(".gemini/antigravity-cli"),
                home.join(".gemini/antigravity"),
                home.join(".gemini/antigravity-ide"),
            ],
        }
    );
}

#[test]
fn store_roots_take_trimmed_overrides() {
    let roots = roots(&[
        ("HOME", "/home/user"),
        ("CLAUDE_CONFIG_DIR", "  /tmp/cc  "),
        ("GROK_HOME", "/g"),
        ("HERMES_HOME", "/h"),
        ("KIMI_CODE_HOME", "/k"),
        ("CODEX_HOME", "/c"),
        ("XDG_DATA_HOME", "/data"),
        ("PI_CODING_AGENT_DIR", "/o"),
    ]);
    assert_eq!(roots.claude, Some(PathBuf::from("/tmp/cc")));
    assert_eq!(roots.grok, Some(PathBuf::from("/g")));
    assert_eq!(roots.hermes, Some(PathBuf::from("/h")));
    assert_eq!(roots.kimi, Some(PathBuf::from("/k")));
    assert_eq!(roots.codex, Some(PathBuf::from("/c")));
    assert_eq!(roots.opencode, Some(PathBuf::from("/data/opencode")));
    assert_eq!(roots.omp, Some(PathBuf::from("/o")));
    assert_eq!(roots.cowork.last(), Some(&PathBuf::from("/tmp/cc")));
}

#[test]
fn store_roots_ignore_blank_overrides_and_need_home_otherwise() {
    let blank = roots(&[("HOME", "/home/user"), ("CLAUDE_CONFIG_DIR", "   ")]);
    assert_eq!(blank.claude, Some(PathBuf::from("/home/user/.claude")));
    assert_eq!(blank.cowork.len(), 2);
    let homeless = roots(&[("CODEX_HOME", "/c")]);
    assert_eq!(homeless.claude, None);
    assert_eq!(homeless.omp, None);
    assert_eq!(homeless.codex, Some(PathBuf::from("/c")));
    assert_eq!(homeless.cowork, Vec::<PathBuf>::new());
}

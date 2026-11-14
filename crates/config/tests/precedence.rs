use std::ffi::OsStr;
use std::path::PathBuf;

use config::{Config, Environment};

fn config(users_file: Option<&str>) -> Result<Config, serde_yaml_ng::Error> {
    let gateway = users_file.map_or_else(String::new, |path| {
        format!("gateway:\n  users_file: {path}\n")
    });
    serde_yaml_ng::from_str(&format!("memory:\n  backend: sqlite\n{gateway}"))
}

fn env(pairs: &[(&str, &str)]) -> Environment {
    Environment::from_vars(
        pairs
            .iter()
            .map(|(name, value)| (OsStr::new(name), OsStr::new(value))),
    )
}

#[test]
fn the_config_file_wins_then_the_current_variable_then_the_legacy_one()
-> Result<(), Box<dyn std::error::Error>> {
    let both = env(&[
        ("RIVETHUB_USERS_FILE", "/home/user/current.json"),
        ("RIVETOS_USERS_FILE", "/home/user/legacy.json"),
    ]);
    assert_eq!(
        config(Some("/home/user/config.json"))?.users_file(&both),
        Some(PathBuf::from("/home/user/config.json")),
        "the config file's own value wins over both variables"
    );
    assert_eq!(
        config(None)?.users_file(&both),
        Some(PathBuf::from("/home/user/current.json")),
        "RIVETHUB_ wins over the legacy name"
    );
    let legacy = env(&[("RIVETOS_USERS_FILE", "/home/user/legacy.json")]);
    assert_eq!(
        config(None)?.users_file(&legacy),
        Some(PathBuf::from("/home/user/legacy.json")),
        "the legacy name is read when nothing else is set"
    );
    assert!(
        legacy
            .legacy_notices()
            .iter()
            .any(|notice| notice.starts_with("RIVETOS_USERS_FILE is deprecated")),
        "{:?}",
        legacy.legacy_notices()
    );
    assert_eq!(config(None)?.users_file(&env(&[])), None);
    Ok(())
}

#[test]
fn every_setting_read_from_both_places_puts_the_config_file_first()
-> Result<(), Box<dyn std::error::Error>> {
    let configured: Config = serde_yaml_ng::from_str(
        "memory:\n  postgres:\n    connection_string: postgres://config@127.0.0.1/hub\n    embed_endpoint: http://127.0.0.1:1/config\n    embed_model: config-model\n",
    )?;
    let variables = env(&[
        ("RIVETHUB_PG_URL", "postgres://current@127.0.0.1/hub"),
        ("RIVETOS_PG_URL", "postgres://legacy@127.0.0.1/hub"),
        ("RIVETHUB_EMBED_URL", "http://127.0.0.1:1/current"),
        ("RIVETHUB_EMBED_MODEL", "current-model"),
    ]);
    assert_eq!(
        configured.memory_postgres_url(&variables).as_deref(),
        Some("postgres://config@127.0.0.1/hub")
    );
    assert_eq!(
        configured.memory_embed_endpoint(&variables).as_deref(),
        Some("http://127.0.0.1:1/config")
    );
    assert_eq!(
        configured.memory_embed_model(&variables).as_deref(),
        Some("config-model")
    );
    let bare = config(None)?;
    assert_eq!(
        bare.memory_postgres_url(&variables).as_deref(),
        Some("postgres://current@127.0.0.1/hub")
    );
    Ok(())
}

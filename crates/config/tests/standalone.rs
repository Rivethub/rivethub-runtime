use std::fs;
use std::path::{Path, PathBuf};

use config::{
    Config, ConfigError, ConfigLocation, Environment, NO_MODEL, ValidationResult,
    standalone_config_yaml,
};

fn temp_home(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rivethub-standalone-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn env_for(home: &Path) -> Result<Environment, Box<dyn std::error::Error>> {
    let Some(text) = home.to_str() else {
        return Err("home is not utf-8".into());
    };
    Ok(Environment::from_vars([("HOME", text)]))
}

#[test]
fn standalone_yaml_omits_operator_sections() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("yaml")?;
    let outcome = standalone_round_trip(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn standalone_round_trip(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let database = home.join("memory.sqlite");
    let yaml = standalone_config_yaml(&database, "0.0.0.0", 5174, false)?;
    assert!(yaml.contains("backend: sqlite"), "{yaml}");
    assert!(yaml.contains("0.0.0.0"), "{yaml}");
    assert!(yaml.contains("\ngateway:\n"), "{yaml}");
    assert!(!yaml.contains("den:"), "{yaml}");
    assert!(!yaml.contains("cloud"), "{yaml}");
    assert!(!yaml.contains("runtime"), "{yaml}");
    assert!(!yaml.contains("agents"), "{yaml}");
    assert!(!yaml.contains("providers"), "{yaml}");
    assert!(!yaml.contains("base_url"), "{yaml}");
    let loaded = load_text(home, "config.yaml", &yaml)?;
    assert!(loaded.config.runtime.is_none());
    assert!(loaded.config.agents.is_none());
    assert!(loaded.config.providers.is_none());
    assert!(loaded.config.sqlite_selected());
    assert!(
        loaded.notices.iter().any(|notice| notice == NO_MODEL),
        "{:?}",
        loaded.notices
    );
    let broken = sqlite_document(&database, "gateway:\n  host: 127.0.0.1\n  port: 0\n");
    let port = load_outcome(home, "port.yaml", &broken)?;
    assert_eq!(
        findings(&validation(port)?),
        [(
            "gateway.port",
            "\"gateway.port\" must be an integer between 1 and 65535",
        )]
    );
    let empty = sqlite_document(&database, "agents: {}\n");
    let agents = load_outcome(home, "agents.yaml", &empty)?;
    assert_eq!(
        findings(&validation(agents)?),
        [("agents", "\"agents\" is empty — define at least one agent")]
    );
    let runtime = sqlite_document(&database, "runtime:\n  workspace: /tmp\n");
    let partial = load_outcome(home, "runtime.yaml", &runtime)?;
    assert_eq!(
        findings(&validation(partial)?),
        [(
            "runtime.default_agent",
            "Missing required field \"runtime.default_agent\"",
        )]
    );
    let mistyped = "gateway:\n  port: \"no\"\n";
    let typed = validation(load_outcome(home, "type.yaml", mistyped)?)?;
    assert!(
        typed
            .errors
            .iter()
            .any(|finding| finding.message.contains("wrong type")),
        "{typed:?}"
    );
    Ok(())
}

#[test]
fn den_is_a_deprecated_name_of_the_gateway_section() -> Result<(), Box<dyn std::error::Error>> {
    let home = temp_home("gateway-section")?;
    let outcome = gateway_section_names(&home);
    fs::remove_dir_all(&home)?;
    outcome
}

fn gateway_section_names(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let database = home.join("memory.sqlite");
    let section = "  host: 127.0.0.1\n  port: 5180\n";
    let gateway = format!("gateway:\n{section}");
    let den = format!("den:\n{section}");
    let current = load_text(home, "gateway.yaml", &sqlite_document(&database, &gateway))?;
    assert_eq!(
        current.config.gateway.as_ref().map(|section| section.port),
        Some(5180)
    );
    assert!(
        current.notices.iter().all(|notice| !notice.contains("den")),
        "{:?}",
        current.notices
    );
    let legacy = load_text(home, "den.yaml", &sqlite_document(&database, &den))?;
    assert_eq!(
        legacy.config.gateway.as_ref().map(|section| section.port),
        Some(5180)
    );
    assert!(
        legacy
            .notices
            .iter()
            .any(|notice| notice
                .starts_with("den is deprecated — rename the config section to gateway")),
        "{:?}",
        legacy.notices
    );
    let legacy_port = sqlite_document(&database, "den:\n  port: 0\n");
    assert_eq!(
        findings(&validation(load_outcome(
            home,
            "den-port.yaml",
            &legacy_port
        )?)?),
        [(
            "gateway.port",
            "\"gateway.port\" must be an integer between 1 and 65535",
        )]
    );
    let both = sqlite_document(&database, &format!("{den}{gateway}"));
    assert_eq!(
        findings(&validation(load_outcome(home, "both.yaml", &both)?)?),
        [(
            "den",
            "den and gateway are both set — den is the deprecated name of the gateway section; keep only gateway.",
        )]
    );
    Ok(())
}

fn sqlite_document(database: &Path, extra: &str) -> String {
    format!(
        "memory:\n  backend: sqlite\n  sqlite:\n    path: \"{}\"\n{extra}",
        database.display()
    )
}

fn load_text(
    home: &Path,
    name: &str,
    yaml: &str,
) -> Result<config::Loaded, Box<dyn std::error::Error>> {
    match load_outcome(home, name, yaml)? {
        Ok(loaded) => Ok(loaded),
        Err(error) => Err(error.into()),
    }
}

fn load_outcome(
    home: &Path,
    name: &str,
    yaml: &str,
) -> Result<Result<config::Loaded, ConfigError>, Box<dyn std::error::Error>> {
    let path = home.join(name);
    fs::write(&path, yaml)?;
    let env = env_for(home)?;
    Ok(Config::load(&env, &ConfigLocation::Current(path)))
}

fn validation(
    outcome: Result<config::Loaded, ConfigError>,
) -> Result<ValidationResult, Box<dyn std::error::Error>> {
    match outcome {
        Err(ConfigError::Validation { result }) => Ok(result),
        Err(error) => Err(error.into()),
        Ok(_) => Err("document loaded".into()),
    }
}

fn findings(result: &ValidationResult) -> Vec<(&str, &str)> {
    result
        .errors
        .iter()
        .map(|finding| (finding.path.as_str(), finding.message.as_str()))
        .collect()
}

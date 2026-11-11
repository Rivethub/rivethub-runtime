use std::path::PathBuf;

use config::{Config, Environment, MappingFile};

fn mapping(text: &str) -> Result<MappingFile, config::ConfigError> {
    let env = Environment::from_vars([("HOME", "/home/user")]);
    let text = format!(
        "{text}runtime:\n  workspace: /srv/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\n"
    );
    Config::from_str(&env, &text)?.config.sanitize_mapping(&env)
}

#[test]
fn without_a_setting_the_mapping_is_the_default_file_under_home() -> Result<(), config::ConfigError>
{
    assert_eq!(
        mapping("memory:\n  capture: {}\n")?,
        MappingFile::Default(PathBuf::from("/home/user/.config/rivethub/redactions.toml"))
    );
    Ok(())
}

#[test]
fn a_configured_mapping_resolves_a_tilde_against_home() -> Result<(), config::ConfigError> {
    assert_eq!(
        mapping("memory:\n  sanitize:\n    mapping: ~/share/rules.toml\n")?,
        MappingFile::Configured(PathBuf::from("/home/user/share/rules.toml"))
    );
    Ok(())
}

#[test]
fn a_mapping_that_is_not_a_path_is_a_type_error() {
    let error = mapping("memory:\n  sanitize:\n    mapping: [rules.toml]\n");
    assert!(
        matches!(&error, Err(config::ConfigError::Validation { result })
            if result.errors.iter().any(|finding| finding.path == "memory.sanitize.mapping")),
        "{error:?}"
    );
}

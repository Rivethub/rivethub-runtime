use std::path::PathBuf;

use config::provider::ProviderBody;
use config::{Config, ConfigLocation, Environment, Loaded, Profile, ValidationResult};

use crate::tree::ConfigCommand;
use crate::{Error, Exit, Report};

pub(crate) async fn load(env: &Environment) -> Result<(ConfigLocation, Loaded), Error> {
    let env = env.clone();
    tokio::task::spawn_blocking(move || {
        let location = config::resolve_config_path(&env)?.ok_or(Error::NoConfig)?;
        let loaded = load_at(&env, &location)?;
        Ok((location, loaded))
    })
    .await?
}

fn load_at(env: &Environment, location: &ConfigLocation) -> Result<Loaded, config::ConfigError> {
    let node_error = match Config::load(env, location, Profile::Node) {
        Ok(loaded) => return Ok(loaded),
        Err(error) => error,
    };
    match Config::load(env, location, Profile::Standalone) {
        Ok(standalone) if standalone.config.sqlite_selected() => Ok(standalone),
        Ok(_) | Err(_) => Err(node_error),
    }
}

pub(crate) async fn run(command: ConfigCommand, env: &Environment) -> Result<Report, Error> {
    match command {
        ConfigCommand::Path => {
            let env = env.clone();
            let location = tokio::task::spawn_blocking(move || config::resolve_config_path(&env))
                .await??
                .ok_or(Error::NoConfig)?;
            Ok(Report::success(format!("{}\n", location.path().display())))
        }
        ConfigCommand::Show => {
            let (_, loaded) = load(env).await?;
            let mut report = Report::success(summary(&loaded.config));
            report.stderr = notices(&loaded);
            Ok(report)
        }
        ConfigCommand::Validate { path } => validate(path, env).await,
    }
}

async fn validate(path: Option<PathBuf>, env: &Environment) -> Result<Report, Error> {
    let env = env.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let location = match path {
            Some(path) => ConfigLocation::Current(path),
            None => config::resolve_config_path(&env)?.ok_or(Error::NoConfig)?,
        };
        Ok::<_, Error>(load_at(&env, &location))
    })
    .await??;
    let (result, stderr) = match loaded {
        Ok(loaded) => {
            let stderr = loaded.notices.join("\n");
            (
                ValidationResult {
                    errors: Vec::new(),
                    warnings: loaded.warnings,
                },
                stderr,
            )
        }
        Err(config::ConfigError::Validation { result }) => (result, String::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(Report {
        stdout: format!("{result}\n"),
        stderr: if stderr.is_empty() {
            stderr
        } else {
            format!("{stderr}\n")
        },
        exit: if result.valid() {
            Exit::Ok
        } else {
            Exit::Failure
        },
        stdout_raw: Vec::new(),
    })
}

pub(crate) fn notices(loaded: &Loaded) -> String {
    let mut lines: Vec<String> = loaded
        .warnings
        .iter()
        .map(|finding| format!("Warning: [{}] {}", finding.path, finding.message))
        .collect();
    lines.extend(loaded.notices.iter().cloned());
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn summary(config: &Config) -> String {
    let mut lines = vec!["Agents:".to_owned()];
    for (name, agent) in config.agents.iter().flatten() {
        lines.push(match &agent.provider {
            Some(provider) => format!("  {name} → {provider}"),
            None => format!("  {name}"),
        });
    }
    lines.push("Providers:".to_owned());
    for (name, provider) in config.providers.iter().flatten() {
        let model = match &provider.body {
            ProviderBody::Openai(provider) => provider.model.as_ref(),
            ProviderBody::Cli(provider) => provider.model.as_ref(),
            ProviderBody::Unknown(provider) => provider.model.as_ref(),
            ProviderBody::Removed(_) => None,
        };
        lines.push(match model {
            Some(model) => format!("  {name} → {model}"),
            None => format!("  {name}"),
        });
    }
    if let Some(runtime) = &config.runtime {
        if let Some(agent) = &runtime.default_agent {
            lines.push(format!("Default agent: {agent}"));
        }
        if let Some(workspace) = &runtime.workspace {
            lines.push(format!("Workspace: {}", workspace.display()));
        }
    }
    format!("{}\n", lines.join("\n"))
}

use std::path::PathBuf;

use config::Config;
use config::Environment;

use crate::Error;

pub(super) use kernel::memory::DumpOrigin as Origin;
pub(super) use kernel::memory::DumpSource as Source;

pub(super) enum Target {
    Postgres(String),
    Sqlite(PathBuf),
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Postgres,
    Either,
}

pub(super) fn resolve(
    env: &Environment,
    sqlite: Option<PathBuf>,
    kind: Kind,
) -> Result<Target, Error> {
    let loaded = load(env)?;
    if matches!(kind, Kind::Either)
        && let Some(path) = sqlite
    {
        return Ok(Target::Sqlite(path));
    }
    if loaded.as_ref().is_some_and(Config::sqlite_selected) {
        if matches!(kind, Kind::Postgres) {
            return Err(Error::PostgresUrlRequired);
        }
        if let Some(path) = loaded.as_ref().and_then(sqlite_path) {
            return Ok(Target::Sqlite(path));
        }
    }
    if let Some(url) = env.postgres_url() {
        return Ok(Target::Postgres(url));
    }
    if let Some(url) = loaded.as_ref().and_then(connection_string) {
        return Ok(Target::Postgres(url));
    }
    if matches!(kind, Kind::Either)
        && let Some(path) = loaded.as_ref().and_then(sqlite_path)
    {
        return Ok(Target::Sqlite(path));
    }
    Err(Error::PostgresUrlRequired)
}

pub(super) fn origin_for_url(url: &str) -> Result<Origin, Error> {
    let host = postgres_host(url)?;
    let source = classify(&host);
    let host = match source {
        Source::Local => machine_name()?,
        Source::Cloud | Source::Datahub => host,
    };
    Ok(Origin { source, host })
}

pub(super) fn sqlite_origin() -> Result<Origin, Error> {
    Ok(Origin {
        source: Source::Local,
        host: machine_name()?,
    })
}

pub(super) fn cloud_url(url: &str) -> Result<bool, Error> {
    Ok(postgres_host(url)? == "rivetos.cloud")
}

pub(super) fn load(env: &Environment) -> Result<Option<Config>, Error> {
    let Some(location) = config::resolve_config_path(env)? else {
        return Ok(None);
    };
    let loaded = config::Config::load(env, &location, config::Profile::Standalone)?;
    Ok(Some(loaded.config))
}

fn connection_string(config: &Config) -> Option<String> {
    let url = config
        .memory
        .as_ref()?
        .postgres
        .as_ref()?
        .connection_string
        .clone()?;
    let trimmed = url.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn sqlite_path(config: &Config) -> Option<PathBuf> {
    let path = config.memory.as_ref()?.sqlite.as_ref()?.path.clone()?;
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn postgres_host(url: &str) -> Result<String, Error> {
    let parsed = url::Url::parse(url).map_err(Error::MemoryUrl)?;
    parsed
        .host_str()
        .map(str::to_owned)
        .ok_or(Error::MemoryHost)
}

fn classify(host: &str) -> Source {
    if host == "rivetos.cloud" {
        Source::Cloud
    } else if host == "datahub" || host.ends_with(".datahub") {
        Source::Datahub
    } else {
        Source::Local
    }
}

fn machine_name() -> Result<String, Error> {
    let raw = nix::unistd::gethostname().map_err(|_| Error::HostUnavailable)?;
    let text = raw.to_str().ok_or(Error::HostName)?;
    if text.is_empty() || text.contains(['\n', '\r']) {
        Err(Error::HostUnavailable)
    } else {
        Ok(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::Source;
    use super::cloud_url;
    use super::origin_for_url;

    #[test]
    fn cloud_host_is_exact_and_userinfo_does_not_hide_it() {
        assert!(cloud_url("postgres://user:secret@rivetos.cloud/db").unwrap());
        assert!(!cloud_url("postgres://rivetos.cloud@127.0.0.1/db").unwrap());
        assert!(!cloud_url("postgres://user@rivetos.cloud.example/db").unwrap());
    }

    #[test]
    fn datahub_hosts_are_tagged_and_other_hosts_are_local() {
        let datahub = origin_for_url("postgres://db.datahub/memory").unwrap();
        assert_eq!(datahub.source, Source::Datahub);
        assert_eq!(datahub.host, "db.datahub");
        let bare = origin_for_url("postgres://datahub/memory").unwrap();
        assert_eq!(bare.source, Source::Datahub);
        let local = origin_for_url("postgres://127.0.0.1/memory").unwrap();
        assert_eq!(local.source, Source::Local);
        assert_ne!(local.host, "");
    }
}

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
    if let Some(path) = sqlite {
        return explicit(path, kind);
    }
    let loaded = load(env)?;
    if loaded.as_ref().is_some_and(Config::sqlite_selected) {
        return selected_sqlite(loaded.as_ref(), env, kind);
    }
    if let Some(url) = env.postgres_url() {
        return Ok(Target::Postgres(url));
    }
    if let Some(url) = loaded.as_ref().and_then(connection_string) {
        return Ok(Target::Postgres(url));
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

pub(super) fn configured_sqlite(env: &Environment) -> Result<Option<PathBuf>, Error> {
    let loaded = load_present(env)?;
    let Some(config) = loaded.as_ref().filter(|config| config.sqlite_selected()) else {
        return Ok(None);
    };
    if let Some(path) = sqlite_path(config) {
        return Ok(Some(crate::sqlite_path::service_sqlite_path(path, env)?));
    }
    if config.pglite_import_pending() {
        return Ok(Some(implicit_sqlite(env)?));
    }
    Ok(None)
}

pub(super) fn load(env: &Environment) -> Result<Option<Config>, Error> {
    let Some(location) = config::resolve_config_path(env)? else {
        return Ok(None);
    };
    let loaded = config::Config::load(env, &location)?;
    Ok(Some(loaded.config))
}

pub(super) fn load_present(env: &Environment) -> Result<Option<Config>, Error> {
    let Some(location) = config::resolve_config_path(env)? else {
        return Ok(None);
    };
    match std::fs::metadata(location.path()) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(source.into()),
        Ok(_) => load(env),
    }
}

pub(super) fn load_for(env: &Environment, explicit: bool) -> Result<Option<Config>, Error> {
    if explicit {
        load_present(env)
    } else {
        load(env)
    }
}

fn explicit(path: PathBuf, kind: Kind) -> Result<Target, Error> {
    match kind {
        Kind::Either => Ok(Target::Sqlite(path)),
        Kind::Postgres => Err(Error::SqliteNotAccepted),
    }
}

fn selected_sqlite(
    config: Option<&Config>,
    env: &Environment,
    kind: Kind,
) -> Result<Target, Error> {
    if matches!(kind, Kind::Postgres) {
        return Err(Error::PostgresUrlRequired);
    }
    if let Some(path) = config.and_then(sqlite_path) {
        return Ok(Target::Sqlite(crate::sqlite_path::service_sqlite_path(
            path, env,
        )?));
    }
    if config.is_some_and(Config::pglite_import_pending) {
        return Ok(Target::Sqlite(implicit_sqlite(env)?));
    }
    Err(Error::LocalDatabase)
}

fn implicit_sqlite(env: &Environment) -> Result<PathBuf, Error> {
    Ok(config::Paths::new(env)?.state_dir().join("memory.sqlite"))
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
    use std::path::Path;

    use config::Environment;

    use super::Kind;
    use super::Source;
    use super::Target;
    use super::origin_for_url;
    use super::resolve;
    use crate::Error;

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

    #[test]
    fn a_postgres_node_without_a_url_does_not_open_the_old_sqlite_file() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let database = dir.path().join("old.sqlite");
        std::fs::write(&database, b"untouched").expect("sqlite placeholder");
        let env = configured(dir.path(), &database, "postgres", None);
        assert!(matches!(
            resolve(&env, None, Kind::Either),
            Err(Error::PostgresUrlRequired)
        ));
        assert_eq!(std::fs::read(&database).expect("file"), b"untouched");
    }

    #[test]
    fn a_relative_sqlite_path_is_resolved_from_the_service_home() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let env = yaml(
            dir.path(),
            "memory:\n  backend: sqlite\n  sqlite:\n    path: memory.sqlite\n",
            None,
        );
        let Ok(Target::Sqlite(path)) = resolve(&env, None, Kind::Either) else {
            panic!("sqlite target");
        };
        assert_eq!(path, dir.path().join("memory.sqlite"));
    }

    #[test]
    fn a_sqlite_node_ignores_a_stray_postgres_url() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let database = dir.path().join("memory.sqlite");
        std::fs::write(&database, b"local").expect("sqlite placeholder");
        let env = configured(
            dir.path(),
            &database,
            "sqlite",
            Some("postgres://127.0.0.1/stray"),
        );
        let Ok(Target::Sqlite(path)) = resolve(&env, None, Kind::Either) else {
            panic!("sqlite target");
        };
        assert_eq!(path, database);
    }

    #[test]
    fn an_environment_url_beats_a_configured_postgres_url() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let env = yaml(
            dir.path(),
            "memory:\n  backend: postgres\n  postgres:\n    connection_string: postgres://db.example/live\n",
            Some("postgres://127.0.0.1/stray"),
        );
        let Ok(Target::Postgres(url)) = resolve(&env, None, Kind::Either) else {
            panic!("postgres target");
        };
        assert_eq!(url, "postgres://127.0.0.1/stray");
        let Ok(Target::Postgres(postgres_only)) = resolve(&env, None, Kind::Postgres) else {
            panic!("postgres target");
        };
        assert_eq!(postgres_only, "postgres://127.0.0.1/stray");
    }

    #[test]
    fn a_postgres_backend_without_a_url_uses_the_environment() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let env = yaml(
            dir.path(),
            "memory:\n  backend: postgres\n",
            Some("postgres://127.0.0.1/from-env"),
        );
        let Ok(Target::Postgres(url)) = resolve(&env, None, Kind::Either) else {
            panic!("postgres target");
        };
        assert_eq!(url, "postgres://127.0.0.1/from-env");
    }

    #[test]
    fn a_sqlite_node_without_a_path_does_not_open_postgres() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let env = yaml(
            dir.path(),
            "memory:\n  backend: sqlite\n",
            Some("postgres://127.0.0.1/stray"),
        );
        assert!(matches!(
            resolve(&env, None, Kind::Either),
            Err(Error::LocalDatabase)
        ));
    }

    #[test]
    fn a_pending_pglite_import_uses_the_state_file() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let env = yaml(
            dir.path(),
            "memory:\n  postgres:\n    embedded: {}\n",
            Some("postgres://127.0.0.1/stray"),
        );
        let expected = dir.path().join(".rivethub").join("memory.sqlite");
        let Ok(Target::Sqlite(path)) = resolve(&env, None, Kind::Either) else {
            panic!("sqlite target");
        };
        assert_eq!(path, expected);
        assert_eq!(
            super::configured_sqlite(&env).expect("configured sqlite"),
            Some(expected)
        );
        assert!(matches!(
            resolve(&env, None, Kind::Postgres),
            Err(Error::PostgresUrlRequired)
        ));
    }

    #[test]
    fn an_explicit_sqlite_file_is_used_before_a_missing_config_is_read() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let database = dir.path().join("scratch.sqlite");
        let missing = dir.path().join("missing.yaml");
        let env = Environment::from_vars([
            (
                std::ffi::OsString::from("HOME"),
                dir.path().as_os_str().to_owned(),
            ),
            (
                std::ffi::OsString::from("RIVETHUB_CONFIG"),
                missing.as_os_str().to_owned(),
            ),
        ]);
        let Ok(Target::Sqlite(path)) = resolve(&env, Some(database.clone()), Kind::Either) else {
            panic!("sqlite target");
        };
        assert_eq!(path, database);
        assert!(matches!(
            resolve(&env, Some(database), Kind::Postgres),
            Err(Error::SqliteNotAccepted)
        ));
    }

    #[test]
    fn an_explicit_sqlite_file_overrides_a_configured_postgres_url() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let database = dir.path().join("override.sqlite");
        let env = yaml(
            dir.path(),
            "memory:\n  postgres:\n    connection_string: postgres://db.example/live\n",
            Some("postgres://127.0.0.1/stray"),
        );
        let Ok(Target::Sqlite(path)) = resolve(&env, Some(database.clone()), Kind::Either) else {
            panic!("sqlite target");
        };
        assert_eq!(path, database);
    }

    fn configured(home: &Path, database: &Path, backend: &str, url: Option<&str>) -> Environment {
        let config = home.join("config.yaml");
        std::fs::write(
            &config,
            format!(
                "memory:\n  backend: {backend}\n  sqlite:\n    path: {}\n",
                database.display()
            ),
        )
        .expect("config");
        let mut pairs = vec![
            (
                std::ffi::OsString::from("HOME"),
                home.as_os_str().to_owned(),
            ),
            (
                std::ffi::OsString::from("RIVETHUB_CONFIG"),
                config.as_os_str().to_owned(),
            ),
        ];
        if let Some(url) = url {
            pairs.push((std::ffi::OsString::from("RIVETHUB_PG_URL"), url.into()));
        }
        Environment::from_vars(pairs)
    }

    fn yaml(home: &Path, body: &str, url: Option<&str>) -> Environment {
        let config = home.join("config.yaml");
        std::fs::write(&config, body).expect("config");
        let mut pairs = vec![
            (
                std::ffi::OsString::from("HOME"),
                home.as_os_str().to_owned(),
            ),
            (
                std::ffi::OsString::from("RIVETHUB_CONFIG"),
                config.as_os_str().to_owned(),
            ),
        ];
        if let Some(url) = url {
            pairs.push((std::ffi::OsString::from("RIVETHUB_PG_URL"), url.into()));
        }
        Environment::from_vars(pairs)
    }
}

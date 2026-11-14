use std::fmt::{self, Display, Write as _};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use super::{NodeKind, Scope, ServiceError};

#[derive(Debug)]
pub struct Unit {
    executable: String,
    environment_file: String,
    scope: Scope,
    kind: NodeKind,
}

impl Unit {
    pub fn new(
        executable: &Path,
        environment_file: &Path,
        scope: Scope,
        kind: NodeKind,
    ) -> Result<Self, ServiceError> {
        Ok(Self {
            executable: quote_path(executable)?.replace('$', "$$"),
            environment_file: environment_assignment(environment_file)?,
            scope,
            kind,
        })
    }

    pub async fn install(self, home: PathBuf) -> Result<PathBuf, ServiceError> {
        tokio::task::spawn_blocking(move || self.write(&home)).await?
    }

    fn write(&self, home: &Path) -> Result<PathBuf, ServiceError> {
        validate_path(home)?;
        let destination = self.scope.unit_path(home);
        let parent = destination
            .parent()
            .ok_or_else(|| ServiceError::Path(destination.clone()))?;
        let root = match self.scope {
            Scope::System => parent,
            Scope::User => home,
        };
        let relative = parent
            .strip_prefix(root)
            .map_err(|_| ServiceError::Path(destination.clone()))?;
        let root = root.canonicalize()?;
        let mut directory = root.clone();
        for component in relative.components() {
            directory.push(component);
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            if !directory.canonicalize()?.starts_with(&root) {
                return Err(ServiceError::Path(directory));
            }
        }
        let name = destination
            .file_name()
            .ok_or_else(|| ServiceError::Path(destination.clone()))?;
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(ServiceError::Exists(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        write!(temporary, "{self}")?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(&path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                ServiceError::Exists(path.clone())
            } else {
                ServiceError::Io(error.error)
            }
        })?;
        File::open(directory)?.sync_all()?;
        Ok(path)
    }
}

impl Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = match self.scope {
            Scope::System => "multi-user.target",
            Scope::User => "default.target",
        };
        write!(
            f,
            "[Unit]\nDescription=RivetHub\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} {}\nEnvironmentFile=-{}\nEnvironment=RIVETHUB_LOG_LEVEL=info\nRestart=on-failure\nRestartSec=5\nRestartPreventExitStatus={}\n\n[Install]\nWantedBy={target}\n",
            self.executable,
            self.kind,
            self.environment_file,
            crate::exit::ALREADY_RUNNING
        )
    }
}

fn validate_path(path: &Path) -> Result<(), ServiceError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(ServiceError::Path(path.to_path_buf()));
    }
    Ok(())
}

fn environment_assignment(path: &Path) -> Result<String, ServiceError> {
    validate_path(path)?;
    let text = path
        .to_str()
        .ok_or_else(|| ServiceError::Path(path.to_path_buf()))?;
    if text
        .chars()
        .any(|character| character.is_ascii_control() || character == '"')
    {
        return Err(ServiceError::EnvironmentFile {
            path: path.to_path_buf(),
        });
    }
    Ok(text.replace('%', "%%"))
}

fn quote_path(path: &Path) -> Result<String, ServiceError> {
    validate_path(path)?;
    let text = path
        .to_str()
        .ok_or_else(|| ServiceError::Path(path.to_path_buf()))?;
    if text.contains('\0') {
        return Err(ServiceError::Path(path.to_path_buf()));
    }
    let mut escaped = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            character if character.is_ascii_control() => {
                write!(escaped, "\\x{:02x}", u32::from(character))
                    .map_err(|_| ServiceError::Path(path.to_path_buf()))?;
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    Ok(escaped)
}

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

    pub(crate) fn place(&self, home: &Path) -> Result<Placement, ServiceError> {
        let rendered = self.to_string();
        let destination = self.scope.unit_path(home);
        match fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Placement::Created(self.write(home)?))
            }
            Err(error) => Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(ServiceError::Exists(destination))
            }
            Ok(metadata) if !metadata.is_file() => Err(ServiceError::Path(destination)),
            Ok(_) => place_existing(&destination, &rendered),
        }
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

pub(crate) enum Placement {
    Created(PathBuf),
    Current(PathBuf),
    Updated { note: String },
    Foreign { message: String },
}

pub(crate) fn ignored_environment_files(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("EnvironmentFile=")?;
            let path = rest.strip_prefix('-').unwrap_or(rest);
            (!path.starts_with('/')).then(|| path.to_owned())
        })
        .collect()
}

fn place_existing(destination: &Path, rendered: &str) -> Result<Placement, ServiceError> {
    let existing = fs::read_to_string(destination)?;
    if existing == rendered {
        return Ok(Placement::Current(destination.to_path_buf()));
    }
    if !generated_shape(&existing) {
        return Ok(Placement::Foreign {
            message: foreign_message(destination, &existing, rendered),
        });
    }
    let backup = backup_path(destination)?;
    fs::copy(destination, &backup)?;
    write_atomic(destination, rendered)?;
    Ok(Placement::Updated {
        note: update_line(destination, &backup, &existing, rendered),
    })
}

fn write_atomic(path: &Path, text: &str) -> Result<(), ServiceError> {
    let directory = path
        .parent()
        .ok_or_else(|| ServiceError::Path(path.to_path_buf()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    write!(temporary, "{text}")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn backup_path(path: &Path) -> Result<PathBuf, ServiceError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ServiceError::Path(path.to_path_buf()))?;
    let parent = path
        .parent()
        .ok_or_else(|| ServiceError::Path(path.to_path_buf()))?;
    let base = format!("{name}.bak");
    for suffix in 0..1000 {
        let candidate = parent.join(if suffix == 0 {
            base.clone()
        } else {
            format!("{base}.{suffix}")
        });
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
    }
    Err(ServiceError::Path(parent.join(base)))
}

fn update_line(path: &Path, backup: &Path, old: &str, new: &str) -> String {
    format!(
        "Updated {} (previous unit kept at {}): {}.\n",
        path.display(),
        backup.display(),
        changes(old, new)
    )
}

fn changes(old: &str, new: &str) -> String {
    let mut parts = Vec::new();
    let old_env = field(old, "EnvironmentFile=");
    let new_env = field(new, "EnvironmentFile=");
    if old_env != new_env {
        if old_env.is_some_and(|value| value.strip_prefix('-').unwrap_or(value).starts_with('"')) {
            parts.push("EnvironmentFile is no longer quoted".to_owned());
        } else if let Some(value) = new_env {
            parts.push(format!("EnvironmentFile is now {value}"));
        }
    }
    if field(old, "RestartPreventExitStatus=").is_none()
        && field(new, "RestartPreventExitStatus=").is_some()
    {
        parts.push(format!(
            "added RestartPreventExitStatus={}",
            crate::exit::ALREADY_RUNNING
        ));
    }
    if field(old, "ExecStart=") != field(new, "ExecStart=")
        && let Some(value) = field(new, "ExecStart=")
    {
        parts.push(format!("ExecStart now runs {value}"));
    }
    if parts.is_empty() {
        parts.push("rewrote the unit to match this rh".to_owned());
    }
    parts.join("; ")
}

fn field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix(prefix))
}

fn foreign_message(path: &Path, existing: &str, generated: &str) -> String {
    format!(
        "The service file {} was edited by hand, so it was left unchanged.\nIt differs from the unit this rh would write:\n{}\nMove it aside and run `rh service init` to regenerate it.",
        path.display(),
        line_diff(existing, generated)
    )
}

fn line_diff(existing: &str, generated: &str) -> String {
    let old: Vec<_> = existing.lines().collect();
    let new: Vec<_> = generated.lines().collect();
    let mut out = Vec::new();
    for index in 0..old.len().max(new.len()) {
        let left = old.get(index).copied();
        let right = new.get(index).copied();
        if left == right {
            continue;
        }
        if let Some(left) = left {
            out.push(format!("  unit: {left}"));
        }
        if let Some(right) = right {
            out.push(format!("  rh:   {right}"));
        }
        if out.len() >= 8 {
            out.push("  ...".to_owned());
            break;
        }
    }
    out.join("\n")
}

fn generated_shape(text: &str) -> bool {
    if !text.ends_with('\n') {
        return false;
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 14 && lines.len() != 15 {
        return false;
    }
    let head = [
        "[Unit]",
        "Description=RivetHub",
        "After=network.target",
        "",
        "[Service]",
        "Type=simple",
    ];
    let Some(prefix) = lines.get(..6) else {
        return false;
    };
    if prefix != head
        || !lines.get(6).is_some_and(|line| exec_slot(line))
        || !lines.get(7).is_some_and(|line| environment_slot(line))
    {
        return false;
    }
    if lines.get(8) != Some(&"Environment=RIVETHUB_LOG_LEVEL=info")
        || lines.get(9) != Some(&"Restart=on-failure")
        || lines.get(10) != Some(&"RestartSec=5")
    {
        return false;
    }
    let prevent = prevent_line();
    let rest = if lines.len() == 15 {
        if lines.get(11).copied() != Some(prevent.as_str()) {
            return false;
        }
        lines.get(12..)
    } else {
        lines.get(11..)
    };
    let Some(rest) = rest else {
        return false;
    };
    rest.len() == 3
        && rest.first().is_some_and(|line| line.is_empty())
        && rest.get(1) == Some(&"[Install]")
        && matches!(
            rest.get(2).copied(),
            Some("WantedBy=multi-user.target" | "WantedBy=default.target")
        )
}

fn prevent_line() -> String {
    format!("RestartPreventExitStatus={}", crate::exit::ALREADY_RUNNING)
}

fn exec_slot(line: &str) -> bool {
    let Some(value) = line.strip_prefix("ExecStart=") else {
        return false;
    };
    let Some((quoted, kind)) = value.rsplit_once(' ') else {
        return false;
    };
    (kind == "start" || kind == "local") && quoted_path(quoted)
}

fn environment_slot(line: &str) -> bool {
    let Some(value) = line.strip_prefix("EnvironmentFile=-") else {
        return false;
    };
    if value.starts_with('"') {
        quoted_path(value)
    } else {
        unquoted_environment(value)
    }
}

fn quoted_path(text: &str) -> bool {
    let Some(inner) = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
    else {
        return false;
    };
    decode_quoted(inner).is_some_and(|path| path.starts_with('/'))
}

fn decode_quoted(inner: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    #[cfg_attr(
                        dylint_lib = "rr_lints",
                        expect(
                            fallback,
                            reason = "PATTERNS FALLBACK: an ExecStart escape that is not a hex byte is not a path this rh generated"
                        )
                    )]
                    let byte = u8::from_str_radix(&hex, 16).ok()?;
                    if !byte.is_ascii() {
                        return None;
                    }
                    out.push(char::from(byte));
                }
                _ => return None,
            }
        } else if character == '"' || character.is_ascii_control() {
            return None;
        } else {
            out.push(character);
        }
    }
    Some(out)
}

fn unquoted_environment(value: &str) -> bool {
    if !value.starts_with('/')
        || value
            .chars()
            .any(|character| character.is_ascii_control() || character == '"')
    {
        return false;
    }
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '%' && chars.next() != Some('%') {
            return false;
        }
    }
    true
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

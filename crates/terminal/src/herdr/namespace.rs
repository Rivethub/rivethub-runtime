use std::collections::BTreeSet;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::error::Error;
use crate::herdr::paths;
use crate::session::RosterKey;
use crate::session::SessionKey;

const STAMP_FILE: &str = "rivethub.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum SkipRule {
    ForeignHerdrNamespace,
    ForeignSessionOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStamp {
    pub namespace: String,
    pub session_key: SessionKey,
    pub user: String,
    pub roster_key: RosterKey,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct HostRoots<'a> {
    pub runtime: &'a Path,
    pub temp: &'a Path,
    pub uid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedSession {
    pub session: SessionKey,
    pub roster_key: RosterKey,
    pub argv: Vec<String>,
    pub socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    pub owned: Vec<OwnedSession>,
    pub inactive: Vec<SessionKey>,
    pub skipped: BTreeSet<SkipRule>,
}

enum Class {
    Owned(OwnedSession),
    Inactive(SessionKey),
    Skip(SkipRule),
}

pub fn note_skip(skipped: &mut BTreeSet<SkipRule>, rule: SkipRule) {
    if skipped.insert(rule) {
        tracing::warn!(%rule, "skipped a herdr session outside this gateway namespace");
    }
}

pub fn namespace_id(config_home: &Path) -> Result<String, Error> {
    let name = config_home
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| escape(config_home, config_home))?;
    let Some(id) = name.strip_prefix("rivethub-") else {
        return Err(escape(config_home, config_home));
    };
    if id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(id.to_owned())
    } else {
        Err(escape(config_home, config_home))
    }
}

#[must_use]
pub fn path_is_inside(root: &Path, child: &Path) -> bool {
    match (lexical(root), lexical(child)) {
        (Some(root), Some(child)) => child.starts_with(&root) && child != root,
        (None, _) | (_, None) => false,
    }
}

pub fn write_stamp(dir: &Path, stamp: &SessionStamp) -> Result<(), Error> {
    let bytes = serde_json::to_vec(stamp).map_err(Error::SessionStamp)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(dir.join(STAMP_FILE))?;
    std::io::Write::write_all(&mut file, &bytes)?;
    Ok(())
}

pub fn session_socket_is_owned(config_home: &Path, socket: &Path) -> Result<bool, Error> {
    if !path_is_inside(&paths::sessions_dir(config_home), socket) {
        return Ok(false);
    }
    if is_symlink(socket)? {
        return Ok(false);
    }
    let root = std::fs::canonicalize(config_home)?;
    let resolved = std::fs::canonicalize(socket)?;
    Ok(resolved.starts_with(root.join("herdr").join("sessions")))
}

pub fn scan(
    state_dir: &Path,
    config_home: &Path,
    owner: &str,
    hosts: &HostRoots<'_>,
) -> Result<Scan, Error> {
    if owner.is_empty() {
        return Err(Error::OwnerUnset);
    }
    let short = paths::is_short_runtime(state_dir, config_home);
    if !covers(state_dir, config_home) && !short {
        return Err(escape(config_home, state_dir));
    }
    let namespace = namespace_id(config_home)?;
    let mut scan = Scan::default();
    let runtime_home = host_home(hosts.runtime, hosts.uid, config_home)?;
    note_host(&mut scan, state_dir, &runtime_home)?;
    let temp_home = hosts.temp.join(config_home_file(config_home)?);
    note_host(&mut scan, state_dir, &temp_home)?;
    let sessions = paths::sessions_dir(config_home);
    if !path_is_inside(config_home, &sessions) {
        return Err(escape(&sessions, config_home));
    }
    match std::fs::canonicalize(&sessions) {
        Ok(resolved) => {
            let anchor = if short { config_home } else { state_dir };
            let root = std::fs::canonicalize(anchor)?;
            if !resolved.starts_with(&root) {
                return Err(escape(&sessions, anchor));
            }
            read_sessions(&sessions, &namespace, owner, &mut scan)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::Io(error)),
    }
    Ok(scan)
}

fn read_sessions(
    sessions: &Path,
    namespace: &str,
    owner: &str,
    scan: &mut Scan,
) -> Result<(), Error> {
    let entries = match std::fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error)),
    };
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        classify_entry(sessions, namespace, owner, &entry, scan)?;
    }
    Ok(())
}

fn classify_entry(
    sessions: &Path,
    namespace: &str,
    owner: &str,
    entry: &std::fs::DirEntry,
    scan: &mut Scan,
) -> Result<(), Error> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B12: refuse a non-UTF-8 directory as a foreign herdr namespace"
        )
    )]
    let Ok(name) = entry.file_name().into_string() else {
        note_skip(&mut scan.skipped, SkipRule::ForeignHerdrNamespace);
        return Ok(());
    };
    let dir = sessions.join(&name);
    if !path_is_inside(sessions, &dir) || is_symlink(&dir)? || !is_directory(&dir)? {
        note_skip(&mut scan.skipped, SkipRule::ForeignHerdrNamespace);
        return Ok(());
    }
    match read_owned(&dir, namespace, owner, &name)? {
        Class::Owned(session) => scan.owned.push(session),
        Class::Inactive(session) => scan.inactive.push(session),
        Class::Skip(rule) => note_skip(&mut scan.skipped, rule),
    }
    Ok(())
}

fn read_owned(dir: &Path, namespace: &str, owner: &str, dir_name: &str) -> Result<Class, Error> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(dir.join(STAMP_FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Class::Skip(SkipRule::ForeignHerdrNamespace));
        }
        Err(error) => return Err(Error::Io(error)),
    };
    let stamp: SessionStamp = serde_json::from_reader(file).map_err(Error::SessionStamp)?;
    if stamp.namespace != namespace || paths::session_dir_name(&stamp.session_key) != dir_name {
        return Ok(Class::Skip(SkipRule::ForeignHerdrNamespace));
    }
    if stamp.user != owner {
        return Ok(Class::Skip(SkipRule::ForeignSessionOwner));
    }
    let socket = dir.join("herdr.sock");
    if !path_is_inside(dir, &socket) || is_symlink(&socket)? {
        return Ok(Class::Skip(SkipRule::ForeignHerdrNamespace));
    }
    match std::fs::symlink_metadata(&socket) {
        Ok(metadata) if metadata.is_dir() => Ok(Class::Inactive(stamp.session_key)),
        Ok(_) => Ok(Class::Owned(OwnedSession {
            session: stamp.session_key,
            roster_key: stamp.roster_key,
            argv: stamp.argv,
            socket,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Class::Inactive(stamp.session_key))
        }
        Err(error) => Err(Error::Io(error)),
    }
}

fn note_host(scan: &mut Scan, state_dir: &Path, candidate: &Path) -> Result<(), Error> {
    if covers(state_dir, candidate) {
        return Ok(());
    }
    match std::fs::symlink_metadata(candidate) {
        Ok(metadata) if metadata.is_dir() || metadata.file_type().is_symlink() => {
            note_skip(&mut scan.skipped, SkipRule::ForeignHerdrNamespace);
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

fn host_home(runtime: &Path, uid: u32, config_home: &Path) -> Result<PathBuf, Error> {
    Ok(runtime
        .join(uid.to_string())
        .join(config_home_file(config_home)?))
}

fn config_home_file(config_home: &Path) -> Result<String, Error> {
    config_home
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| escape(config_home, config_home))
}

fn covers(root: &Path, child: &Path) -> bool {
    match (lexical(root), lexical(child)) {
        (Some(root), Some(child)) => child.starts_with(&root),
        (None, _) | (_, None) => false,
    }
}

fn lexical(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if component == Component::CurDir {
            continue;
        }
        normalized.push(component);
    }
    Some(normalized)
}

fn is_symlink(path: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Io(error)),
    }
}

fn is_directory(path: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Io(error)),
    }
}

fn escape(path: &Path, root: &Path) -> Error {
    Error::NamespaceEscape {
        path: Box::from(path),
        root: Box::from(root),
    }
}

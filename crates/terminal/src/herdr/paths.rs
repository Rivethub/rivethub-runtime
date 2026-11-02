use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

use crate::error::Error;
use crate::session::SessionKey;

pub const SUN_PATH_MAX: usize = 107;

#[must_use]
fn hash8(state_dir: &Path, port: u16) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state_dir.as_os_str().as_encoded_bytes());
    hasher.update(b":");
    hasher.update(port.to_string().as_bytes());
    let digest = hasher.finalize();
    let prefix: Vec<u8> = digest.iter().take(4).copied().collect();
    hex::encode(prefix)
}

#[must_use]
pub fn session_dir_name(key: &SessionKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_str().as_bytes());
    let digest = hasher.finalize();
    let prefix: Vec<u8> = digest.iter().take(6).copied().collect();
    format!("d{}", hex::encode(prefix))
}

#[must_use]
pub fn config_home_name(state_dir: &Path, port: u16) -> String {
    format!("rivethub-{}", hash8(state_dir, port))
}

#[must_use]
pub fn resolve_config_home(state_dir: &Path, port: u16) -> PathBuf {
    let nested = state_dir
        .join("run")
        .join(config_home_name(state_dir, port));
    if session_socket(&nested, "d0123456789ab").is_ok() && client_socket(&nested).is_ok() {
        nested
    } else {
        std::env::temp_dir().join(config_home_name(state_dir, port))
    }
}

#[must_use]
pub fn is_short_runtime(state_dir: &Path, config_home: &Path) -> bool {
    let Some(name) = config_home.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(id) = name.strip_prefix("rivethub-") else {
        return false;
    };
    if id.len() != 8 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let Some(parent) = config_home.parent() else {
        return false;
    };
    if parent != std::env::temp_dir() {
        return false;
    }
    (0..=u16::MAX).any(|port| hash8(state_dir, port) == id)
}

#[must_use]
pub fn sessions_dir(config_home: &Path) -> PathBuf {
    config_home.join("herdr").join("sessions")
}

pub fn session_socket(config_home: &Path, session: &str) -> Result<PathBuf, Error> {
    assert_sun_path(
        &config_home
            .join("herdr")
            .join("sessions")
            .join(session)
            .join("herdr.sock"),
    )
}

pub fn client_socket(config_home: &Path) -> Result<PathBuf, Error> {
    assert_sun_path(&config_home.join("herdr-client.sock"))
}

fn assert_sun_path(path: &Path) -> Result<PathBuf, Error> {
    let len = path.as_os_str().len();
    if len > SUN_PATH_MAX {
        return Err(Error::SocketPathTooLong {
            len,
            path: Box::from(path),
        });
    }
    Ok(path.to_path_buf())
}

#[derive(Debug)]
pub struct Starting {
    pub child: tokio::process::Child,
    pub said: PathBuf,
}

#[must_use]
pub fn server_errors(socket: &Path) -> PathBuf {
    socket.with_file_name("rivethub-server.err")
}

pub async fn wait_for_socket(
    socket: &Path,
    timeout: std::time::Duration,
    session: &str,
    mut server: Option<&mut Starting>,
) -> Result<(), Error> {
    let timeout_ms = timeout.as_millis();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::fs::try_exists(socket).await? {
            return Ok(());
        }
        if let Some(starting) = server.as_deref_mut()
            && let Some(status) = starting.child.try_wait()?
        {
            let said = tokio::fs::read_to_string(&starting.said).await?;
            return Err(Error::HerdrServerExited {
                session: session.to_owned(),
                status,
                said: said.trim().to_owned(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::HerdrServerTimeout {
                socket: Box::from(socket),
                timeout_ms,
                session: session.to_owned(),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::assert_sun_path;
    use super::client_socket;
    use super::config_home_name;
    use super::hash8;
    use super::resolve_config_home;
    use super::session_dir_name;
    use super::session_socket;
    use crate::error::Error;
    use crate::session::SessionKey;

    fn long_dir() -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from("/tmp");
        for _ in 0..24 {
            path.push("state-directory-segment-0123456789");
        }
        path
    }

    #[test]
    fn config_home_uses_the_recorded_hash_shape() {
        assert_eq!(
            config_home_name(std::path::Path::new("/home/user/.rivethub"), 5174),
            format!(
                "rivethub-{}",
                hash8(std::path::Path::new("/home/user/.rivethub"), 5174)
            ),
        );
        assert_eq!(
            hash8(std::path::Path::new("/home/user/.rivethub"), 5174).len(),
            8
        );
    }

    #[test]
    fn config_home_stays_under_the_state_root_when_a_host_runtime_dir_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let name = config_home_name(&state, 5174);
        let runtime_root = tmp.path().join("run-user");
        std::fs::create_dir_all(runtime_root.join("1000").join(&name)).expect("mkdir");
        std::fs::create_dir_all(tmp.path().join("tmp").join(&name)).expect("mkdir");
        let resolved = resolve_config_home(&state, 5174);
        assert_eq!(resolved, state.join("run").join(name));
        assert!(resolved.starts_with(&state));
        assert_ne!(
            resolved,
            runtime_root
                .join("1000")
                .join(config_home_name(&state, 5174))
        );
    }

    #[test]
    fn session_name_is_the_hashed_d_prefix() {
        let key = SessionKey::new("claude-code:00000000-0000-4000-8000-000000000001").expect("key");
        let name = session_dir_name(&key);
        assert_eq!(name.len(), 13);
        assert!(name.starts_with('d'));
    }

    #[test]
    fn socket_paths_sit_under_the_config_home() {
        let home = std::path::Path::new("/run/user/1000/rivethub-0123abcd");
        let socket = session_socket(home, "d0123456789ab").expect("socket");
        assert_eq!(
            socket,
            std::path::PathBuf::from(
                "/run/user/1000/rivethub-0123abcd/herdr/sessions/d0123456789ab/herdr.sock"
            )
        );
        let client = client_socket(home).expect("socket");
        assert_eq!(
            client,
            std::path::PathBuf::from("/run/user/1000/rivethub-0123abcd/herdr-client.sock")
        );
    }

    #[test]
    fn a_long_state_directory_keeps_sockets_under_the_path_limit() {
        let state = long_dir();
        let home = resolve_config_home(&state, 5174);
        assert!(client_socket(&home).is_ok());
        assert!(session_socket(&home, "d0123456789ab").is_ok());
        assert!(home.starts_with(std::env::temp_dir()));
        assert!(super::is_short_runtime(&state, &home));
    }

    #[test]
    fn overlong_socket_paths_are_a_typed_error() {
        let error = session_socket(&long_dir(), "d0123456789ab").expect_err("too long");
        let Error::SocketPathTooLong { len, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert!(len > 107);
    }

    #[test]
    fn the_sun_path_limit_is_107_bytes() {
        let mut path = std::path::PathBuf::from("/tmp");
        path.push("a".repeat(101));
        assert_eq!(path.as_os_str().len(), 106);
        assert!(assert_sun_path(&path).is_ok());
        path.push("b");
        assert_eq!(path.as_os_str().len(), 108);
        assert!(assert_sun_path(&path).is_err());
    }
}

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use capture::sanitize::KeyError;
use capture::sanitize::key::PrivateFile;
use capture::seal::ShareServer;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use kernel::memory::share::desk::TokenStore;
use serde::Deserialize;
use serde::Serialize;

use super::Login;
use super::ServerError;
use super::TeamLogin;
use super::Tokens;

pub(crate) const TOKEN_STORE_VARIABLE: &str = "RIVETHUB_TOKEN_STORE";
const KEYRING_SERVICE: &str = "rivethub share login";
const STORE_NAME: &str = "share server logins";
const STORE_CAP: u64 = 256 * 1024;
const SECRET_SERVICE_SESSION_BUS: &str = "DBUS_SESSION_BUS_ADDRESS";
const KEYRING_IS_SECRET_SERVICE: bool = cfg!(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))
));

pub(crate) trait Vault: Send + Sync + std::fmt::Debug {
    fn read(&self, server: &str) -> Result<Option<String>, keyring::Error>;
    fn write(&self, server: &str, secret: &str) -> Result<(), keyring::Error>;
    fn erase(&self, server: &str) -> Result<(), keyring::Error>;
}

#[derive(Debug)]
struct OsKeyring;

impl Vault for OsKeyring {
    fn read(&self, server: &str) -> Result<Option<String>, keyring::Error> {
        match keyring::Entry::new(KEYRING_SERVICE, server)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write(&self, server: &str, secret: &str) -> Result<(), keyring::Error> {
        keyring::Entry::new(KEYRING_SERVICE, server)?.set_password(secret)
    }

    fn erase(&self, server: &str) -> Result<(), keyring::Error> {
        match keyring::Entry::new(KEYRING_SERVICE, server)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoginStore {
    file: PathBuf,
    vault: Option<Arc<dyn Vault>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Listed {
    Device {
        scope: String,
        expires_at: DateTime<Utc>,
    },
    Team {
        user: String,
    },
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Records {
    logins: BTreeMap<ShareServer, Record>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Record {
    Device(DeviceRecord),
    Team(TeamRecord),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceRecord {
    expires_at: DateTime<Utc>,
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamRecord {
    user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Secret {
    Device {
        access_token: String,
        refresh_token: String,
    },
    Team {
        team_token: String,
    },
}

enum Held<T> {
    File(T),
    Keyring,
}

impl Record {
    fn listed(&self) -> Listed {
        match self {
            Self::Device(device) => Listed::Device {
                scope: device.scope.clone(),
                expires_at: device.expires_at,
            },
            Self::Team(team) => Listed::Team {
                user: team.user.clone(),
            },
        }
    }

    const fn in_keyring(&self) -> bool {
        matches!(
            self,
            Self::Device(DeviceRecord {
                access_token: None,
                refresh_token: None,
                ..
            }) | Self::Team(TeamRecord {
                team_token: None,
                ..
            })
        )
    }

    fn held(&self, server: &str) -> Result<Held<Secret>, ServerError> {
        let partial = || {
            ServerError::Store(format!(
                "the login for {server} holds one of its two tokens; run rh share login {server}"
            ))
        };
        Ok(match self {
            Self::Device(DeviceRecord {
                access_token: Some(access_token),
                refresh_token: Some(refresh_token),
                ..
            }) => Held::File(Secret::Device {
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
            }),
            Self::Device(DeviceRecord {
                access_token: None,
                refresh_token: None,
                ..
            })
            | Self::Team(TeamRecord {
                team_token: None, ..
            }) => Held::Keyring,
            Self::Device(DeviceRecord { .. }) => return Err(partial()),
            Self::Team(TeamRecord {
                team_token: Some(team_token),
                ..
            }) => Held::File(Secret::Team {
                team_token: team_token.clone(),
            }),
        })
    }

    fn login(self, secret: Secret, server: &str) -> Result<Login, ServerError> {
        match (self, secret) {
            (
                Self::Device(device),
                Secret::Device {
                    access_token,
                    refresh_token,
                },
            ) => Ok(Login::Device(Tokens {
                access_token,
                refresh_token,
                expires_at: device.expires_at,
                scope: device.scope,
            })),
            (Self::Team(team), Secret::Team { team_token }) => Ok(Login::Team(TeamLogin {
                team_token,
                user: team.user,
            })),
            (Self::Device(_) | Self::Team(_), Secret::Device { .. } | Secret::Team { .. }) => {
                Err(ServerError::Store(format!(
                    "the OS keyring holds a different kind of login for {server}; run rh share login {server}"
                )))
            }
        }
    }

    fn split(login: &Login, inline: bool) -> (Self, Secret) {
        match login {
            Login::Device(tokens) => (
                Self::Device(DeviceRecord {
                    expires_at: tokens.expires_at,
                    scope: tokens.scope.clone(),
                    access_token: inline.then(|| tokens.access_token.clone()),
                    refresh_token: inline.then(|| tokens.refresh_token.clone()),
                }),
                Secret::Device {
                    access_token: tokens.access_token.clone(),
                    refresh_token: tokens.refresh_token.clone(),
                },
            ),
            Login::Team(team) => (
                Self::Team(TeamRecord {
                    user: team.user.clone(),
                    team_token: inline.then(|| team.team_token.clone()),
                }),
                Secret::Team {
                    team_token: team.team_token.clone(),
                },
            ),
        }
    }
}

impl LoginStore {
    pub(crate) async fn choose(env: &Environment, file: PathBuf) -> Result<Self, ServerError> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B9 (note 140): with RIVETHUB_TOKEN_STORE unset, a keyring that does not open means the 0600 file; the_environment_chooses_the_file_store_without_touching_a_keyring pins it"
            )
        )]
        let vault = match env.text(TOKEN_STORE_VARIABLE) {
            Some("file") => None,
            Some("keyring") => Some(os_keyring().await.map_err(ServerError::Keyring)?),
            Some(other) => {
                return Err(ServerError::Store(format!(
                    "{TOKEN_STORE_VARIABLE} is {other:?}; use keyring or file"
                )));
            }
            None if KEYRING_IS_SECRET_SERVICE && env.get(SECRET_SERVICE_SESSION_BUS).is_none() => {
                None
            }
            None => os_keyring()
                .await
                .inspect_err(|reason| {
                    tracing::debug!(%reason, "no OS keyring; share tokens stay in the login file");
                })
                .ok(),
        };
        Ok(Self { file, vault })
    }

    #[cfg(test)]
    pub(crate) fn with_vault(file: PathBuf, vault: Option<Arc<dyn Vault>>) -> Self {
        Self { file, vault }
    }

    pub(crate) const fn kind(&self) -> TokenStore {
        if self.vault.is_some() {
            TokenStore::Keyring
        } else {
            TokenStore::File
        }
    }

    pub(crate) fn file(&self) -> &Path {
        &self.file
    }

    pub(crate) fn list(&self) -> Result<BTreeMap<String, Listed>, ServerError> {
        Ok(self
            .records()?
            .logins
            .iter()
            .map(|(server, record)| (server.to_string(), record.listed()))
            .collect())
    }

    pub(crate) async fn load(&self, server: &ShareServer) -> Result<Login, ServerError> {
        let host = server.as_str();
        let record = self
            .records()?
            .logins
            .remove(server)
            .ok_or_else(|| ServerError::LoggedOut(server.clone()))?;
        let secret = match record.held(host)? {
            Held::File(secret) => secret,
            Held::Keyring => {
                let vault = self.vault.clone().ok_or_else(|| {
                    ServerError::Store(format!(
                        "the login for {host} is in the OS keyring, but {TOKEN_STORE_VARIABLE}=file keeps this process out of it"
                    ))
                })?;
                let owned = host.to_owned();
                let text = blocking(move || vault.read(&owned))
                    .await?
                    .ok_or_else(|| ServerError::LoggedOut(server.clone()))?;
                serde_json::from_str(&text)
                    .map_err(|error| ServerError::Store(error.to_string()))?
            }
        };
        record.login(secret, host)
    }

    pub(crate) async fn save(
        &self,
        server: &ShareServer,
        login: &Login,
    ) -> Result<(), ServerError> {
        let (record, secret) = Record::split(login, self.vault.is_none());
        if let Some(vault) = self.vault.clone() {
            let text = serde_json::to_string(&secret)
                .map_err(|error| ServerError::Store(error.to_string()))?;
            let owned = server.as_str().to_owned();
            blocking(move || vault.write(&owned, &text)).await?;
        }
        let mut records = self.records()?;
        records.logins.insert(server.clone(), record);
        self.write(&records)
    }

    pub(crate) async fn forget(&self, server: &ShareServer) -> Result<(), ServerError> {
        let mut records = self.records()?;
        let Some(record) = records.logins.remove(server) else {
            return Ok(());
        };
        if let (true, Some(vault)) = (record.in_keyring(), self.vault.clone()) {
            let owned = server.as_str().to_owned();
            blocking(move || vault.erase(&owned)).await?;
        }
        self.write(&records)
    }

    fn records(&self) -> Result<Records, ServerError> {
        let bytes = match PrivateFile::new(&self.file, STORE_NAME, STORE_CAP).read() {
            Ok(bytes) => bytes,
            Err(KeyError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Records::default());
            }
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&bytes).map_err(|error| ServerError::Store(error.to_string()))
    }

    fn write(&self, records: &Records) -> Result<(), ServerError> {
        let parent = self.file.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut partial = tempfile::Builder::new()
            .prefix(".share-logins-")
            .tempfile_in(parent)?;
        let text =
            serde_json::to_vec(records).map_err(|error| ServerError::Store(error.to_string()))?;
        partial.write_all(&text)?;
        partial.as_file().sync_all()?;
        partial
            .persist(&self.file)
            .map_err(|error| ServerError::Io(error.error))?;
        Ok(())
    }
}

async fn os_keyring() -> Result<Arc<dyn Vault>, String> {
    tokio::task::spawn_blocking(|| {
        keyring::Entry::store_status()
            .as_ref()
            .map_err(ToString::to_string)
            .copied()
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(Arc::new(OsKeyring))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, keyring::Error> + Send + 'static,
) -> Result<T, ServerError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(std::io::Error::other)?
        .map_err(|error| ServerError::Keyring(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[derive(Debug, Default)]
    struct MemoryVault(parking_lot::Mutex<BTreeMap<String, String>>);

    impl Vault for MemoryVault {
        fn read(&self, server: &str) -> Result<Option<String>, keyring::Error> {
            Ok(self.0.lock().get(server).cloned())
        }

        fn write(&self, server: &str, secret: &str) -> Result<(), keyring::Error> {
            self.0.lock().insert(server.to_owned(), secret.to_owned());
            Ok(())
        }

        fn erase(&self, server: &str) -> Result<(), keyring::Error> {
            self.0.lock().remove(server);
            Ok(())
        }
    }

    const OLD_FILE: &str = r#"{"logins":{"rivethub.cloud":{"access_token":"rhca_x","refresh_token":"rhcr_x","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"},"team.example.com":{"team_token":"rht_x","user":"phil"}}}"#;

    fn server(host: &str) -> ShareServer {
        host.parse().expect("server")
    }

    fn old_file(dir: &Path) -> PathBuf {
        let file = dir.join("share-logins.json");
        std::fs::write(&file, OLD_FILE).expect("old logins");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        file
    }

    fn team(token: &str) -> Login {
        Login::Team(TeamLogin {
            team_token: token.to_owned(),
            user: "phil".to_owned(),
        })
    }

    fn token_of(login: &Login) -> &str {
        match login {
            Login::Device(tokens) => &tokens.refresh_token,
            Login::Team(team) => &team.team_token,
        }
    }

    #[tokio::test]
    async fn the_file_store_reads_old_files_and_round_trips() {
        let dir = tempfile::tempdir().expect("dir");
        let store = LoginStore::with_vault(old_file(dir.path()), None);
        assert_eq!(store.kind(), TokenStore::File);

        let cloud = store.load(&server("rivethub.cloud")).await.expect("cloud");
        assert_eq!(token_of(&cloud), "rhcr_x");
        let listed = store.list().expect("list");
        assert_eq!(
            listed.get("team.example.com"),
            Some(&Listed::Team {
                user: "phil".to_owned()
            })
        );

        store
            .save(&server("other.example.com"), &team("rht_other"))
            .await
            .expect("save");
        let read = store
            .load(&server("other.example.com"))
            .await
            .expect("load");
        assert_eq!(token_of(&read), "rht_other");
        let mode = std::fs::metadata(store.file())
            .expect("file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");

        store
            .forget(&server("other.example.com"))
            .await
            .expect("forget");
        assert!(matches!(
            store.load(&server("other.example.com")).await,
            Err(ServerError::LoggedOut(_))
        ));
        assert!(!format!("{cloud:?}").contains("rhcr_x"));
    }

    #[tokio::test]
    async fn the_keyring_store_keeps_only_non_secret_fields_in_the_file() {
        let dir = tempfile::tempdir().expect("dir");
        let vault = Arc::new(MemoryVault::default());
        let store = LoginStore::with_vault(old_file(dir.path()), Some(vault.clone()));
        assert_eq!(store.kind(), TokenStore::Keyring);
        let cloud = server("rivethub.cloud");

        let old = store
            .load(&cloud)
            .await
            .expect("an old file entry still reads");
        store
            .save(&cloud, &old)
            .await
            .expect("moved into the keyring");

        let text = std::fs::read_to_string(store.file()).expect("file");
        assert!(!text.contains("rhcr_x"), "{text}");
        assert!(text.contains("drops:write"), "{text}");
        assert!(
            vault
                .0
                .lock()
                .get("rivethub.cloud")
                .is_some_and(|secret| secret.contains("rhcr_x"))
        );
        let read = store.load(&cloud).await.expect("from the keyring");
        assert_eq!(token_of(&read), "rhcr_x");

        store.forget(&cloud).await.expect("logout");
        assert!(
            vault.0.lock().get("rivethub.cloud").is_none(),
            "logout erases the keyring entry"
        );
        assert!(
            !std::fs::read_to_string(store.file())
                .expect("file")
                .contains("rivethub.cloud")
        );
    }

    #[tokio::test]
    async fn a_keyring_login_in_a_file_only_process_says_so() {
        let dir = tempfile::tempdir().expect("dir");
        let file = old_file(dir.path());
        let keyring = LoginStore::with_vault(file.clone(), Some(Arc::new(MemoryVault::default())));
        keyring
            .save(&server("team.example.com"), &team("rht_y"))
            .await
            .expect("save");

        let refused = LoginStore::with_vault(file, None)
            .load(&server("team.example.com"))
            .await;

        assert!(
            matches!(&refused, Err(ServerError::Store(reason)) if reason.contains("RIVETHUB_TOKEN_STORE=file")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn the_environment_chooses_the_file_store_without_touching_a_keyring() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("share-logins.json");
        let forced = Environment::from_vars([(TOKEN_STORE_VARIABLE, "file")]);
        let chosen = LoginStore::choose(&forced, file.clone())
            .await
            .expect("file");
        assert_eq!(chosen.kind(), TokenStore::File);

        if KEYRING_IS_SECRET_SERVICE {
            let busless = Environment::from_vars([("HOME", "/nonexistent")]);
            let chosen = LoginStore::choose(&busless, file.clone())
                .await
                .expect("file");
            assert_eq!(chosen.kind(), TokenStore::File);
        }

        let wrong = Environment::from_vars([(TOKEN_STORE_VARIABLE, "vault")]);
        assert!(matches!(
            LoginStore::choose(&wrong, file).await,
            Err(ServerError::Store(_))
        ));
    }
}

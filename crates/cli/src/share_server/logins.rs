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
    pinned: bool,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Forgot {
    Absent,
    Clean,
    SecretKept(String),
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
        Ok(Self {
            file,
            vault,
            pinned: env.text(TOKEN_STORE_VARIABLE) == Some("keyring"),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_vault(file: PathBuf, vault: Option<Arc<dyn Vault>>) -> Self {
        Self {
            file,
            vault,
            pinned: false,
        }
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
        let in_keyring = match self.vault.clone() {
            None => false,
            Some(vault) => {
                let (_, secret) = Record::split(login, false);
                let text = serde_json::to_string(&secret)
                    .map_err(|error| ServerError::Store(error.to_string()))?;
                let owned = server.as_str().to_owned();
                self.keep_in_keyring(blocking(move || vault.write(&owned, &text)).await)?
            }
        };
        let (record, _) = Record::split(login, !in_keyring);
        let mut records = self.records()?;
        records.logins.insert(server.clone(), record);
        self.write(&records)
    }

    fn keep_in_keyring(&self, written: Result<(), ServerError>) -> Result<bool, ServerError> {
        match written {
            Ok(()) => Ok(true),
            Err(error) if self.pinned => Err(error),
            Err(error) => {
                tracing::debug!(%error, "the OS keyring refused the login; it stays in the login file");
                Ok(false)
            }
        }
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

    pub(crate) async fn forget_record(&self, server: &ShareServer) -> Result<Forgot, ServerError> {
        let mut records = self.records()?;
        let Some(record) = records.logins.remove(server) else {
            return Ok(Forgot::Absent);
        };
        let kept = if record.in_keyring() {
            self.erase_secret(server).await.err()
        } else {
            None
        };
        self.write(&records)?;
        Ok(kept.map_or(Forgot::Clean, Forgot::SecretKept))
    }

    async fn erase_secret(&self, server: &ShareServer) -> Result<(), String> {
        let Some(vault) = self.vault.clone() else {
            return Err(format!(
                "{TOKEN_STORE_VARIABLE}=file keeps this process out of the OS keyring"
            ));
        };
        let owned = server.as_str().to_owned();
        tokio::task::spawn_blocking(move || vault.erase(&owned))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
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
#[path = "logins_tests.rs"]
mod tests;

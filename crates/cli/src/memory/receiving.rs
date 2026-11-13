use std::path::PathBuf;
use std::time::SystemTime;

use capture::seal::DropId;
use capture::seal::OpenError;
use capture::seal::Opened;
use capture::seal::SealError;
use capture::seal::ShareCode;
use capture::seal::ShareServer;
use capture::sign::PublicKey;
use capture::sign::TrustError;
use capture::sign::TrustStore;
use capture::sign::TrustedSender;
use capture::sign::Verification;
use chrono::DateTime;
use chrono::Utc;
use config::Environment;
use thiserror::Error;
use wire::capture::Receipt;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::dropbox::DropBox;
use super::dropbox::DropError;
use super::dropbox::MAX_DROP_BYTES;
use super::inbox::Inbox;
use super::inbox::Slot;
use super::share::ShareError;
use crate::Error;
use crate::share_server::ServerError;
use crate::share_server::ShareServers;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("bundle {0} was already received on this machine; nothing was downloaded")]
    AlreadyReceived(DropId),
    #[error(transparent)]
    Setup(Box<Error>),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error(transparent)]
    Drop(#[from] DropError),
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Share(#[from] ShareError),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<FetchError> for Error {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Setup(error) => *error,
            FetchError::Server(error) => Self::ShareServer(error),
            FetchError::Drop(error) => Self::Drop(error),
            FetchError::Open(error) => Self::Sharing(error.into()),
            FetchError::Trust(error) => Self::Sharing(error.into()),
            FetchError::Share(error) => Self::Sharing(error),
            FetchError::Config(error) => Self::Config(error),
            FetchError::Io(error) => Self::Io(error),
            error @ FetchError::AlreadyReceived(_) => Self::Io(std::io::Error::other(error)),
        }
    }
}

#[derive(Debug)]
pub struct Fetched {
    inbox: Inbox,
    slot: Slot,
    pub id: DropId,
    pub server: ShareServer,
    pub downloaded: bool,
    pub opened: Opened,
}

impl Fetched {
    #[must_use]
    pub fn bundle(&self) -> &SessionBundle {
        &self.opened.bundle
    }

    #[must_use]
    pub fn ciphertext_path(&self) -> PathBuf {
        self.inbox.ciphertext_path(&self.slot)
    }

    #[must_use]
    pub fn receipt(&self, at: DateTime<Utc>) -> Receipt {
        receipt(&self.id, at, &self.opened.verification)
    }

    pub fn mark_received(&self) -> std::io::Result<()> {
        self.inbox.mark_received(&self.slot)
    }
}

pub async fn fetch(
    env: &Environment,
    code: ShareCode,
    downloading: impl FnOnce(&ShareServer) -> std::io::Result<()>,
) -> Result<Fetched, FetchError> {
    let inbox = Inbox::in_state(&capture::share::state_home(env)?);
    inbox.prune(SystemTime::now())?;
    let ShareCode { server, id, key } = code;
    let slot = Slot::of(&server, &id);
    if inbox.was_received(&slot) {
        return Err(FetchError::AlreadyReceived(id));
    }
    let (ciphertext, downloaded) = if let Some(kept) = inbox.read(&slot, MAX_DROP_BYTES)? {
        (kept, false)
    } else {
        let servers = ShareServers::from_env(env)
            .await
            .map_err(|error| FetchError::Setup(Box::new(error)))?;
        let drops = DropBox::on(&servers, &server).await?;
        downloading(&server)?;
        let partial = inbox.partial()?;
        drops.download(&id, partial.as_file().try_clone()?).await?;
        inbox.keep(&slot, partial)?;
        let kept = inbox
            .read(&slot, MAX_DROP_BYTES)?
            .ok_or_else(|| std::io::Error::other("the inbox lost the download"))?;
        (kept, true)
    };
    let trust = trust_store(env)?;
    let opened = tokio::task::spawn_blocking(move || capture::seal::open(ciphertext, &key, &trust))
        .await
        .map_err(std::io::Error::other)??;
    screen(&opened.bundle)?;
    Ok(Fetched {
        inbox,
        slot,
        id,
        server,
        downloaded,
        opened,
    })
}

fn screen(bundle: &SessionBundle) -> Result<(), ShareError> {
    if bundle.manifest.sanitization == Sanitization::Raw {
        return Err(ShareError::Raw);
    }
    let flags = capture::sanitize::open_flags(bundle).map_err(SealError::from)?;
    if flags.withheld > 0 {
        return Err(ShareError::Withheld(flags.withheld));
    }
    Ok(())
}

pub(super) fn receipt(id: &DropId, at: DateTime<Utc>, verification: &Verification) -> Receipt {
    Receipt {
        bundle_id: id.to_string(),
        received_at: at,
        verification: verification.kind(),
        key_fingerprint: verification.key().map(PublicKey::fingerprint),
        checks: None,
    }
}

pub(super) fn trust_path(env: &Environment) -> Result<PathBuf, config::ConfigError> {
    Ok(TrustStore::path_in(&capture::share::config_home(env)?))
}

fn trust_store(env: &Environment) -> Result<TrustStore, FetchError> {
    Ok(TrustStore::load(&trust_path(env)?)?)
}

pub fn trusted_senders(env: &Environment) -> Result<Vec<TrustedSender>, FetchError> {
    Ok(trust_store(env)?.senders().to_vec())
}

pub fn trust_sender(
    env: &Environment,
    name: &str,
    key: PublicKey,
) -> Result<TrustedSender, FetchError> {
    let path = trust_path(env)?;
    let mut trust = TrustStore::load(&path)?;
    trust.add(name, key)?;
    trust.save(&path)?;
    Ok(TrustedSender {
        name: name.trim().to_owned(),
        key,
    })
}

pub fn untrust_sender(
    env: &Environment,
    name: &str,
    key: Option<&PublicKey>,
) -> Result<usize, FetchError> {
    let path = trust_path(env)?;
    let mut trust = TrustStore::load(&path)?;
    let removed = trust.remove(name.trim(), key);
    trust.save(&path)?;
    Ok(removed)
}

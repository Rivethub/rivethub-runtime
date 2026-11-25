use jiff::Timestamp;
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
use config::Environment;
use thiserror::Error;
use wire::capture::Receipt;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::dropbox::DropBox;
use super::dropbox::DropError;
use super::dropbox::Incoming;
use super::dropbox::MAX_DROP_BYTES;
use super::inbox::Inbox;
use super::inbox::Slot;
use super::local_drop;
use super::local_drop::Local;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Inbox,
    ThisNode,
    Server(ShareServer),
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inbox => formatter.write_str("the inbox"),
            Self::ThisNode => formatter.write_str("this node's drop box"),
            Self::Server(server) => server.fmt(formatter),
        }
    }
}

#[derive(Debug)]
pub struct Fetched {
    inbox: Inbox,
    slot: Slot,
    pub id: DropId,
    pub source: Source,
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
    pub fn receipt(&self, at: Timestamp) -> Receipt {
        receipt(&self.id, at, &self.opened.verification)
    }

    pub fn mark_received(&self) -> std::io::Result<()> {
        self.inbox.mark_received(&self.slot)
    }
}

pub async fn fetch(
    env: &Environment,
    code: ShareCode,
    from: Option<ShareServer>,
    downloading: impl FnOnce(&Source) -> std::io::Result<()>,
) -> Result<Fetched, FetchError> {
    let inbox = Inbox::in_state(&capture::share::state_home(env)?);
    inbox.prune(SystemTime::now())?;
    let ShareCode { server, id, key } = code;
    let slot = Slot::of(&server, &id);
    if inbox.was_received(&slot) {
        return Err(FetchError::AlreadyReceived(id));
    }
    let (ciphertext, source) = if let Some(kept) = inbox.read(&slot, MAX_DROP_BYTES)? {
        (kept, Source::Inbox)
    } else {
        let wanted = Wanted {
            inbox: &inbox,
            slot: &slot,
            id: &id,
        };
        let source = wanted.download(env, from, server, downloading).await?;
        let kept = inbox
            .read(&slot, MAX_DROP_BYTES)?
            .ok_or_else(|| std::io::Error::other("the inbox lost the download"))?;
        (kept, source)
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
        source,
        opened,
    })
}

struct Wanted<'a> {
    inbox: &'a Inbox,
    slot: &'a Slot,
    id: &'a DropId,
}

impl Wanted<'_> {
    async fn download(
        &self,
        env: &Environment,
        from: Option<ShareServer>,
        embedded: ShareServer,
        downloading: impl FnOnce(&Source) -> std::io::Result<()>,
    ) -> Result<Source, FetchError> {
        let partial = self.inbox.partial()?;
        let local = match from {
            Some(_) => Local::NotHere,
            None => local_drop::locate(env, self.id).await?,
        };
        let gateway = match local {
            Local::Claimed { mut file, bytes } => {
                if bytes > MAX_DROP_BYTES as u64 {
                    return Err(DropError::TooLarge.into());
                }
                downloading(&Source::ThisNode)?;
                std::io::copy(&mut file, &mut partial.as_file().try_clone()?)?;
                self.inbox.keep(self.slot, partial)?;
                return Ok(Source::ThisNode);
            }
            Local::ThroughGateway => true,
            Local::NotHere => false,
        };
        let servers = ShareServers::from_env(env)
            .await
            .map_err(|error| FetchError::Setup(Box::new(error)))?;
        if gateway {
            let drops = DropBox::on_this_node(env, &servers).await?;
            if let Some(incoming) = served_here(drops.request(self.id).await)? {
                return self.save(incoming, Source::ThisNode, downloading).await;
            }
        }
        let host = from.unwrap_or(embedded);
        let drops = DropBox::on(&servers, &host).await?;
        let incoming = drops.request(self.id).await?;
        self.save(incoming, Source::Server(host), downloading).await
    }

    async fn save(
        &self,
        incoming: Incoming,
        source: Source,
        downloading: impl FnOnce(&Source) -> std::io::Result<()>,
    ) -> Result<Source, FetchError> {
        downloading(&source)?;
        let partial = self.inbox.partial()?;
        incoming.save(partial.as_file().try_clone()?).await?;
        self.inbox.keep(self.slot, partial)?;
        Ok(source)
    }
}

fn served_here(probe: Result<Incoming, DropError>) -> Result<Option<Incoming>, DropError> {
    match probe {
        Ok(incoming) => Ok(Some(incoming)),
        Err(DropError::Gone(_) | DropError::Server(ServerError::Answered(401))) => Ok(None),
        Err(DropError::Server(ServerError::Http(error))) if error.is_connect() => Ok(None),
        Err(error) => Err(error),
    }
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

pub(super) fn receipt(id: &DropId, at: Timestamp, verification: &Verification) -> Receipt {
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

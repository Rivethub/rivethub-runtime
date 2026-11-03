use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::ca::{
    CaError, CertChainPem, CertificateAuthority, CrlPem, CsrPem, EnrollmentToken, Principal,
    SignAuthority,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Error;
use crate::leaf;
use crate::mint::{self, Endpoint, ProcessEnv};
use crate::paths;
use crate::roster::{self, Roster};

#[derive(Debug)]
pub struct Settings {
    pub url: Url,
    pub root: PathBuf,
    pub password_file: PathBuf,
    pub roster: Option<PathBuf>,
    pub environment: ProcessEnv,
}

#[derive(Debug)]
struct Issued {
    intermediate: Option<String>,
    serials: BTreeMap<String, Vec<String>>,
}

#[derive(Debug)]
pub struct Authority {
    remote: stepca::Authority,
    endpoint: Endpoint,
    roster: Option<PathBuf>,
    environment: ProcessEnv,
    cancel: CancellationToken,
    issued: Mutex<Issued>,
}

struct Prepared {
    pem: String,
    endpoint: Endpoint,
    roster: Option<PathBuf>,
    environment: ProcessEnv,
}

impl Authority {
    pub async fn connect(settings: Settings, cancel: CancellationToken) -> Result<Self, Error> {
        let prepared = tokio::task::spawn_blocking(move || prepare(settings)).await??;
        let config = stepca::Config::new(prepared.endpoint.url.clone(), prepared.pem)?;
        let remote = stepca::Authority::connect(config)?.with_cancellation(cancel.clone());
        Ok(Self {
            remote,
            endpoint: prepared.endpoint,
            roster: prepared.roster,
            environment: prepared.environment,
            cancel,
            issued: Mutex::new(Issued {
                intermediate: None,
                serials: BTreeMap::new(),
            }),
        })
    }

    async fn token_for(&self, principal: &Principal) -> Result<String, CaError> {
        let roster = self.roster_for(principal).await?;
        let plan = mint::plan_principal(principal, roster.as_ref()).map_err(refused_local)?;
        let minted = mint::mint(&plan, &self.endpoint, &self.environment, &self.cancel)
            .await
            .map_err(unavailable)?;
        Ok(minted.token)
    }

    async fn roster_for(&self, principal: &Principal) -> Result<Option<Roster>, CaError> {
        if matches!(principal, Principal::Device { .. }) {
            return Ok(None);
        }
        let Some(path) = self.roster.clone() else {
            return Err(CaError::Refused("operator roster is required".to_owned()));
        };
        let text = tokio::task::spawn_blocking(move || paths::read_roster(&path))
            .await
            .map_err(unavailable)?
            .map_err(unavailable)?;
        roster::parse_roster(&text).map(Some).map_err(refused_local)
    }

    async fn chain_from(&self, intermediate: Option<String>) -> Result<Vec<String>, CaError> {
        let mut chain = Vec::new();
        if let Some(pem) = intermediate {
            chain.push(pem);
        }
        let roots = self.remote.roots().await.map_err(from_step)?;
        chain.extend(roots);
        if chain.is_empty() {
            Err(CaError::Unavailable(Box::new(Error::Root)))
        } else {
            Ok(chain)
        }
    }

    fn remember(&self, signed: &stepca::SignedCertificate) -> Result<(), Error> {
        let identity = leaf::identity(&signed.leaf)?;
        let mut issued = self.issued.lock();
        issued.intermediate = Some(signed.intermediate.clone());
        for principal in identity.principals {
            match issued.serials.get_mut(&principal) {
                Some(serials) => serials.push(identity.serial.clone()),
                None => {
                    issued
                        .serials
                        .insert(principal, vec![identity.serial.clone()]);
                }
            }
        }
        drop(issued);
        Ok(())
    }

    fn serials_of(&self, principal: &str) -> Result<Vec<String>, CaError> {
        let issued = self.issued.lock();
        match issued.serials.get(principal) {
            Some(serials) if !serials.is_empty() => Ok(serials.clone()),
            Some(_) | None => Err(CaError::Refused(
                "no certificate is recorded for this principal".to_owned(),
            )),
        }
    }
}

fn prepare(settings: Settings) -> Result<Prepared, Error> {
    let (root, pem) = paths::read_root(&settings.root)?;
    let password_file = paths::contained_file(&settings.password_file, false)?;
    let roster = match settings.roster {
        Some(path) => Some(paths::contained_file(&path, true)?),
        None => None,
    };
    Ok(Prepared {
        pem,
        endpoint: Endpoint {
            url: settings.url,
            root,
            password_file,
        },
        roster,
        environment: settings.environment,
    })
}

fn from_step(error: stepca::Error) -> CaError {
    match error {
        stepca::Error::Refused { message, .. } => CaError::Refused(message),
        stepca::Error::Rejected { message, .. } => CaError::InvalidCsr(message),
        stepca::Error::Csr => CaError::InvalidCsr(stepca::Error::Csr.to_string()),
        stepca::Error::Token => CaError::Refused(stepca::Error::Token.to_string()),
        other @ (stepca::Error::Address(_)
        | stepca::Error::Root
        | stepca::Error::Serial
        | stepca::Error::HolderCertificate
        | stepca::Error::Holder(_)
        | stepca::Error::Unavailable { .. }
        | stepca::Error::Cancelled
        | stepca::Error::Clock
        | stepca::Error::Transport(_)
        | stepca::Error::Invalid(_)
        | stepca::Error::Untrusted(_)) => CaError::Unavailable(Box::new(other)),
    }
}

fn refused_local(error: Error) -> CaError {
    let message = error.to_string();
    match error {
        Error::StepCa(inner) => from_step(inner),
        Error::RosterRequired
        | Error::RosterLine { .. }
        | Error::RosterNode { .. }
        | Error::RosterAgent { .. }
        | Error::RosterAddress { .. }
        | Error::UnknownNode(_)
        | Error::UnknownAgent(_)
        | Error::DevicePrincipal
        | Error::Serial
        | Error::StepToken
        | Error::MeshRegistry => CaError::Refused(message),
        other @ (Error::Io(_)
        | Error::Task(_)
        | Error::CaFile
        | Error::CaEscape
        | Error::CaTraversal
        | Error::Missing
        | Error::StepFailed { .. }
        | Error::StepEncoding
        | Error::StepOutput
        | Error::StepPipe
        | Error::Cancelled
        | Error::Certificate
        | Error::Root) => CaError::Unavailable(Box::new(other)),
    }
}

fn unavailable(error: impl std::error::Error + Send + Sync + 'static) -> CaError {
    CaError::Unavailable(Box::new(error))
}

#[async_trait]
impl CertificateAuthority for Authority {
    async fn issue_token(&self, principal: Principal) -> Result<EnrollmentToken, CaError> {
        self.token_for(&principal).await.map(EnrollmentToken)
    }

    async fn sign(&self, csr: CsrPem, authority: SignAuthority) -> Result<CertChainPem, CaError> {
        let token = match authority {
            SignAuthority::Token(token) => token.0,
            SignAuthority::Renewal(principal) => self.token_for(&principal).await?,
        };
        let signed = self.remote.sign(&csr.0, &token).await.map_err(from_step)?;
        self.remember(&signed).map_err(unavailable)?;
        let intermediate = self.issued.lock().intermediate.clone();
        let chain = self.chain_from(intermediate).await?;
        Ok(CertChainPem {
            leaf: signed.leaf,
            chain,
        })
    }

    async fn chain(&self) -> Result<Vec<String>, CaError> {
        let intermediate = self.issued.lock().intermediate.clone();
        self.chain_from(intermediate).await
    }

    async fn crl(&self) -> Result<CrlPem, CaError> {
        self.remote.crl().await.map(CrlPem).map_err(from_step)
    }

    async fn revoke(&self, principal: Principal) -> Result<(), CaError> {
        let uri = principal.spiffe_uri()?;
        let serials = self.serials_of(&uri)?;
        for serial in &serials {
            mint::revoke_serial(serial, &self.endpoint, &self.environment, &self.cancel)
                .await
                .map_err(unavailable)?;
        }
        self.issued.lock().serials.remove(&uri);
        Ok(())
    }
}

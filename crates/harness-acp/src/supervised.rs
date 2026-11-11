use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use config::Config;
use config::provider::CliProvider;
use kernel::harness::{ApprovalDecision, HarnessOffering};
use kernel::harness::{
    HarnessAdapter, HarnessAdapterError, SessionEvent, SessionOption, SessionSpec, TurnInput,
};
use kernel::model::ProviderRecovery;
use kernel::model::{Degradation, ProviderHealth, ProviderStatus};
use tokio::sync::mpsc::Receiver;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::error::port;
use crate::headless::Prepared;
use crate::{AcpError, AcpHarnessAdapter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub first: Duration,
    pub ceiling: Duration,
}

impl Retry {
    pub const STANDARD: Self = Self {
        first: Duration::from_secs(1),
        ceiling: Duration::from_secs(60),
    };

    fn after(self, delay: Duration) -> Duration {
        delay.saturating_mul(2).min(self.ceiling)
    }
}

#[derive(Debug, Clone)]
pub struct Supervisor {
    pub shutdown: CancellationToken,
    pub tasks: TaskTracker,
    pub retry: Retry,
}

#[derive(Debug)]
pub struct HeadlessLaunch {
    provider: String,
    launch: Result<Prepared, AcpError>,
}

impl HeadlessLaunch {
    pub async fn prepare(
        config: &Config,
        harness: HarnessId,
        provider: &CliProvider,
        search: Vec<PathBuf>,
        id: String,
    ) -> Self {
        Self {
            provider: id,
            launch: crate::headless::prepare(config, harness, provider, search).await,
        }
    }

    #[must_use]
    pub fn with_discovery_dir(mut self, directory: PathBuf) -> Self {
        if let Ok(prepared) = &mut self.launch {
            prepared.discover_in(directory);
        }
        self
    }

    pub async fn start(self, supervisor: Supervisor) -> HeadlessHarness {
        let Self { provider, launch } = self;
        let prepared = match launch {
            Ok(prepared) => prepared,
            Err(error) => {
                let discovery = degraded(&provider, error);
                return Publisher::open(provider, discovery).0;
            }
        };
        let error = match prepared.discover().await {
            Ok(advertisement) => {
                let ready = Discovery::Ready(Arc::new(prepared.ready(advertisement)));
                return Publisher::open(provider, ready).0;
            }
            Err(error) => error,
        };
        let recovery = error.recovery();
        let (harness, publisher) = Publisher::open(provider.clone(), degraded(&provider, error));
        if recovery == ProviderRecovery::Retrying {
            supervisor.tasks.spawn(recover(
                provider,
                prepared,
                publisher,
                supervisor.shutdown,
                supervisor.retry,
            ));
        }
        harness
    }
}

#[derive(Debug, Clone)]
enum Discovery {
    Ready(Arc<AcpHarnessAdapter>),
    Degraded(Degradation),
}

impl Discovery {
    fn status(&self) -> ProviderStatus {
        match self {
            Self::Ready(_) => ProviderStatus::Ready,
            Self::Degraded(degradation) => ProviderStatus::Degraded(degradation.clone()),
        }
    }
}

#[derive(Debug)]
struct Publisher {
    discovery: watch::Sender<Discovery>,
    status: watch::Sender<ProviderStatus>,
}

impl Publisher {
    fn open(provider: String, discovery: Discovery) -> (HeadlessHarness, Self) {
        let (status, status_rx) = watch::channel(discovery.status());
        let (discovery, discovery_rx) = watch::channel(discovery);
        (
            HeadlessHarness {
                provider,
                discovery: discovery_rx,
                status: status_rx,
            },
            Self { discovery, status },
        )
    }

    fn publish(&self, discovery: Discovery) {
        let status = discovery.status();
        self.discovery.send_replace(discovery);
        self.status.send_replace(status);
    }
}

fn degraded(provider: &str, error: AcpError) -> Discovery {
    let recovery = error.recovery();
    tracing::error!(provider, %recovery, "provider {provider} is degraded: {error}");
    Discovery::Degraded(Degradation {
        recovery,
        cause: Arc::new(error),
    })
}

async fn recover(
    provider: String,
    prepared: Prepared,
    publisher: Publisher,
    shutdown: CancellationToken,
    retry: Retry,
) {
    let mut delay = retry.first;
    loop {
        let attempt = tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            attempt = async {
                tokio::time::sleep(delay).await;
                prepared.discover().await
            } => attempt,
        };
        let error = match attempt {
            Ok(advertisement) => {
                tracing::info!(
                    provider,
                    "provider {provider} recovered: its harness answered ACP discovery"
                );
                publisher.publish(Discovery::Ready(Arc::new(prepared.ready(advertisement))));
                return;
            }
            Err(error) => error,
        };
        let recovery = error.recovery();
        if recovery == ProviderRecovery::Permanent {
            publisher.publish(degraded(&provider, error));
            return;
        }
        tracing::debug!(provider, %error, "provider discovery retry failed");
        publisher.publish(Discovery::Degraded(Degradation {
            recovery,
            cause: Arc::new(error),
        }));
        delay = retry.after(delay);
    }
}

#[derive(Debug)]
pub struct HeadlessHarness {
    provider: String,
    discovery: watch::Receiver<Discovery>,
    status: watch::Receiver<ProviderStatus>,
}

impl HeadlessHarness {
    #[must_use]
    pub fn health(&self) -> ProviderHealth {
        ProviderHealth {
            id: self.provider.clone(),
            status: self.status.clone(),
        }
    }

    fn ready(&self) -> Result<Arc<AcpHarnessAdapter>, HarnessAdapterError> {
        let discovery = self.discovery.borrow().clone();
        match discovery {
            Discovery::Ready(adapter) => Ok(adapter),
            Discovery::Degraded(degradation) => Err(port(AcpError::ProviderDegraded {
                provider: self.provider.clone(),
                degradation,
            })),
        }
    }
}

#[async_trait]
impl HarnessAdapter for HeadlessHarness {
    async fn release(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        self.ready()?.release(session).await
    }

    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError> {
        self.ready()?.new_session(spec).await
    }

    async fn prompt(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<(), HarnessAdapterError> {
        self.ready()?.prompt(session, input).await
    }

    async fn cancel(&self, session: &SessionId) -> Result<(), HarnessAdapterError> {
        self.ready()?.cancel(session).await
    }

    async fn answer_permission(
        &self,
        session: &SessionId,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError> {
        self.ready()?
            .answer_permission(session, request_id, decision)
            .await
    }

    async fn set_option(
        &self,
        session: &SessionId,
        option: SessionOption,
    ) -> Result<(), HarnessAdapterError> {
        self.ready()?.set_option(session, option).await
    }

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<Receiver<SessionEvent>, HarnessAdapterError> {
        self.ready()?.events(session).await
    }

    async fn offerings(&self) -> Result<Vec<HarnessOffering>, HarnessAdapterError> {
        self.ready()?.offerings().await
    }
}

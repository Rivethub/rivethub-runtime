use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tokio_util::task::TaskTracker;
use wire::plugin::PluginId;
use wire::plugin::host::{PluginLogStream, PluginRecord, PluginState};
use wire::plugin::manifest::Permission;
use wire::plugin::process::PluginEvent;

use super::{
    Entry, HostError, Live, PairedDevice, PluginHost, Supervisor, events, jobs, registry,
    supervisor,
};
use crate::Gateway;

pub async fn start(gateway: &Arc<Gateway>) -> Result<(), HostError> {
    let Some(host) = gateway.plugins() else {
        return Ok(());
    };
    if host.sessions.set(Arc::clone(gateway.sessions())).is_err() {
        return Err(HostError::Conflict(
            "the plug-in host is already started".into(),
        ));
    }
    if let Some(terminal) = gateway.terminal.as_ref()
        && host.terminal.set(Arc::downgrade(terminal)).is_err()
    {
        tracing::debug!("plug-in host already knows the terminal service");
    }
    if let Some(owner) = gateway.tenancy.local_user()
        && host.owner.set(owner.clone()).is_err()
    {
        tracing::debug!("plug-in host already knows the node's owner");
    }
    registry::private_folder(&host.settings.root).await?;
    let key = super::tools::issue(&host.tools_token()).await?;
    if host.tools_key.set(key).is_err() {
        return Err(HostError::Conflict(
            "the plug-in host is already started".into(),
        ));
    }
    let api = super::api::bind(&host.api_socket()).await?;
    let tools = super::api::bind(&host.tools_socket()).await?;
    let inner = gateway.router(false);
    host.tasks.spawn(super::api::serve(
        Arc::clone(host),
        inner,
        api,
        host.shutdown.child_token(),
    ));
    host.tasks.spawn(super::tools::serve(
        Arc::clone(host),
        tools,
        host.shutdown.child_token(),
    ));
    if let Some(summaries) = gateway.summaries.as_ref() {
        host.tasks
            .spawn(forward_summaries(Arc::clone(host), summaries.written()));
    }
    let ids: Vec<PluginId> = host.entries.lock().keys().cloned().collect();
    for id in ids {
        host.refresh(&id).await;
    }
    Ok(())
}

async fn forward_summaries(
    host: Arc<PluginHost>,
    mut written: tokio::sync::broadcast::Receiver<kernel::memory::WrittenSummary>,
) {
    loop {
        let summary = tokio::select! {
            () = host.shutdown.cancelled() => return,
            summary = written.recv() => summary,
        };
        match summary {
            Ok(summary) => {
                #[cfg_attr(
                    dylint_lib = "rr_lints",
                    expect(
                        fallback,
                        reason = "PATTERNS FALLBACK: a real absence, as note 56 of the contract leaves sessionId out when the key is not a canonical session id"
                    )
                )]
                let session_id = summary.session_key.parse().ok();
                host.publish(&PluginEvent::SummaryWritten {
                    summary_id: summary.id.to_string(),
                    session_id,
                    kind: crate::memory_body::summary_kind_body(summary.kind),
                    level: summary.depth.unsigned_abs(),
                });
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "plug-in summary events lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

impl PluginHost {
    pub async fn close(&self) {
        self.shutdown.cancel();
        let ids: Vec<PluginId> = self.entries.lock().keys().cloned().collect();
        for id in ids {
            self.stop_supervisor(&id).await;
        }
        self.tasks.close();
        self.tasks.wait().await;
        for socket in [self.api_socket(), self.tools_socket()] {
            if let Err(error) = tokio::fs::remove_file(&socket).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, path = %socket.display(), "plug-in socket not removed");
            }
        }
    }

    pub(super) async fn persist(&self) -> Result<(), HostError> {
        let snapshot = self
            .entries
            .lock()
            .iter()
            .map(|(id, entry)| (id.clone(), entry.stored.clone()))
            .collect();
        registry::save(&self.settings.root, snapshot).await
    }

    pub(crate) async fn install(&self, path: &Path) -> Result<PluginRecord, HostError> {
        let _operation = self.operations.lock().await;
        if !path.is_absolute() {
            return Err(HostError::Invalid(
                "path must be an absolute directory".into(),
            ));
        }
        let directory = tokio::fs::canonicalize(path)
            .await
            .map_err(|error| HostError::Invalid(format!("{}: {error}", path.display())))?;
        let manifest = registry::read_manifest(&directory).await?;
        let id = manifest.id.clone();
        if let Some(entry) = self.entries.lock().get(&id) {
            return Err(HostError::Conflict(if entry.stored.path == directory {
                format!("{} is already installed; reload it", id.as_ref())
            } else {
                format!(
                    "{} is installed from {}",
                    id.as_ref(),
                    entry.stored.path.display()
                )
            }));
        }
        let folder = self.folder(&id);
        tokio::fs::create_dir_all(folder.join("data")).await?;
        tokio::fs::create_dir_all(folder.join("logs")).await?;
        self.entries.lock().insert(
            id.clone(),
            Entry {
                stored: registry::Stored {
                    path: directory,
                    manifest,
                    enabled: false,
                    granted: Vec::new(),
                    installed_at: Utc::now(),
                    settings: std::collections::BTreeMap::new(),
                },
                live: Live::new(PluginState::Disabled),
            },
        );
        self.persist().await?;
        self.log(&id, PluginLogStream::Host, "installed".to_owned());
        self.changed(&id);
        self.record(&id)
    }

    pub(crate) async fn enable(
        self: &Arc<Self>,
        plugin: &PluginId,
        approved: &[Permission],
        device: Result<PairedDevice, HostError>,
    ) -> Result<PluginRecord, HostError> {
        let _operation = self.operations.lock().await;
        let requested = self.read(plugin, |entry| entry.stored.manifest.permissions.clone())?;
        self.refuse_unconfinable(&requested)?;
        let (granted, widened) = self.read(plugin, |entry| {
            super::rules::approve(
                &entry.stored.manifest.permissions,
                &entry.stored.granted,
                approved,
            )
            .map(|granted| {
                let widened = granted
                    .iter()
                    .any(|permission| !entry.stored.granted.contains(permission));
                (granted, widened)
            })
        })??;
        if widened {
            device?;
        }
        let running = self.update(plugin, |entry| {
            entry.stored.granted = granted;
            entry.stored.enabled = true;
            entry.live.supervisor.is_some()
        })?;
        self.persist().await?;
        if !running {
            self.start_supervisor(plugin);
        }
        self.record(plugin)
    }

    pub(crate) async fn disable(&self, plugin: &PluginId) -> Result<PluginRecord, HostError> {
        let _operation = self.operations.lock().await;
        self.update(plugin, |entry| entry.stored.enabled = false)?;
        self.persist().await?;
        self.stop_supervisor(plugin).await;
        self.set_state(plugin, PluginState::Disabled, None);
        self.record(plugin)
    }

    pub(crate) async fn reload(
        self: &Arc<Self>,
        plugin: &PluginId,
    ) -> Result<PluginRecord, HostError> {
        let _operation = self.operations.lock().await;
        let path = self.read(plugin, |entry| entry.stored.path.clone())?;
        let manifest = registry::read_manifest(&path).await?;
        if &manifest.id != plugin {
            return Err(HostError::Invalid(format!(
                "id: the manifest now names {}",
                manifest.id.as_ref()
            )));
        }
        self.stop_supervisor(plugin).await;
        self.touch(plugin, |entry| {
            entry.stored.granted =
                super::rules::narrowed(&manifest.permissions, &entry.stored.granted);
            entry.stored.manifest = manifest;
        });
        self.persist().await?;
        self.log(plugin, PluginLogStream::Host, "reloaded".to_owned());
        self.resume(plugin);
        self.record(plugin)
    }

    pub(crate) async fn remove(&self, plugin: &PluginId, keep_data: bool) -> Result<(), HostError> {
        let _operation = self.operations.lock().await;
        self.read(plugin, |_| ())?;
        self.stop_supervisor(plugin).await;
        self.entries.lock().remove(plugin);
        self.persist().await?;
        if !keep_data {
            match tokio::fs::remove_dir_all(self.folder(plugin)).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.changed(plugin);
        Ok(())
    }

    async fn refresh(self: &Arc<Self>, plugin: &PluginId) {
        let Some(path) = self.peek(plugin, |entry| entry.stored.path.clone()) else {
            return;
        };
        match registry::read_manifest(&path).await {
            Ok(manifest) if &manifest.id == plugin => {
                self.touch(plugin, |entry| {
                    entry.stored.granted =
                        super::rules::narrowed(&manifest.permissions, &entry.stored.granted);
                    entry.stored.manifest = manifest;
                });
                if let Err(error) = self.persist().await {
                    tracing::warn!(%error, "plug-in registry not saved after a refresh");
                }
                self.resume(plugin);
            }
            Ok(manifest) => self.set_state(
                plugin,
                PluginState::Invalid,
                Some(format!(
                    "id: the manifest now names {}",
                    manifest.id.as_ref()
                )),
            ),
            Err(error) => self.set_state(plugin, PluginState::Invalid, Some(error.to_string())),
        }
    }

    fn resume(self: &Arc<Self>, plugin: &PluginId) {
        let Some((enabled, waiting)) = self.peek(plugin, |entry| {
            (
                entry.stored.enabled,
                !super::rules::pending(&entry.stored.manifest.permissions, &entry.stored.granted)
                    .is_empty(),
            )
        }) else {
            return;
        };
        match (enabled, waiting) {
            (false, _) => self.set_state(plugin, PluginState::Disabled, None),
            (true, true) => self.set_state(
                plugin,
                PluginState::AwaitingApproval,
                Some("the manifest asks for more permissions; approve them to restart".into()),
            ),
            (true, false) => self.start_supervisor(plugin),
        }
    }

    fn start_supervisor(self: &Arc<Self>, plugin: &PluginId) {
        let stop = self.shutdown.child_token();
        let queue = Arc::new(events::EventQueue::default());
        let tasks = TaskTracker::new();
        let supervisor = Supervisor {
            stop: stop.clone(),
            events: Arc::clone(&queue),
            tasks: tasks.clone(),
        };
        let declared = self.peek_mut(plugin, |entry| {
            if entry.live.supervisor.is_some() {
                return None;
            }
            entry.live.supervisor = Some(supervisor);
            entry.live.state = PluginState::Starting;
            entry.live.error = None;
            Some(entry.stored.manifest.jobs.clone())
        });
        let Some(Some(declared)) = declared else {
            return;
        };
        tasks.spawn(supervisor::supervise(
            Arc::clone(self),
            plugin.clone(),
            stop.clone(),
        ));
        tasks.spawn(events::deliver(
            Arc::clone(self),
            plugin.clone(),
            queue,
            stop.clone(),
        ));
        tasks.spawn(jobs::schedule(
            Arc::clone(self),
            plugin.clone(),
            declared,
            stop,
        ));
        tasks.close();
        self.changed(plugin);
    }

    async fn stop_supervisor(&self, plugin: &PluginId) {
        let supervisor = self
            .peek_mut(plugin, |entry| entry.live.supervisor.take())
            .flatten();
        if let Some(supervisor) = supervisor {
            supervisor.stop.cancel();
            supervisor.tasks.wait().await;
        }
    }
}

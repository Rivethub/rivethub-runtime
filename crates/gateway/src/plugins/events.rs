use jiff::Timestamp;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wire::plugin::PluginId;
use wire::plugin::host::PluginLogStream;
use wire::plugin::process::{PluginEvent, PluginEventDelivery};

use super::PluginHost;

const CAPACITY: usize = 1000;
const TRIES: u32 = 5;
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
const RUNNING_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub(super) struct EventQueue {
    state: Mutex<QueueState>,
    arrived: Notify,
}

#[derive(Debug, Default)]
struct QueueState {
    waiting: VecDeque<PluginEvent>,
    missed: u64,
    seq: u64,
}

impl EventQueue {
    pub(super) fn push(&self, event: PluginEvent) {
        let mut state = self.state.lock();
        if state.waiting.len() >= CAPACITY {
            state.waiting.pop_front();
            state.missed += 1;
        }
        state.waiting.push_back(event);
        drop(state);
        self.arrived.notify_one();
    }

    async fn next(&self) -> PluginEventDelivery {
        loop {
            let arrived = self.arrived.notified();
            if let Some(delivery) = self.stamp() {
                return delivery;
            }
            arrived.await;
        }
    }

    fn stamp(&self) -> Option<PluginEventDelivery> {
        let mut state = self.state.lock();
        let event = state.waiting.pop_front()?;
        state.seq += 1;
        Some(PluginEventDelivery {
            seq: state.seq,
            at: Timestamp::now(),
            missed: std::mem::take(&mut state.missed),
            event,
        })
    }

    fn lost(&self) {
        self.state.lock().missed += 1;
    }
}

pub(super) async fn deliver(
    host: Arc<PluginHost>,
    plugin: PluginId,
    queue: Arc<EventQueue>,
    stop: CancellationToken,
) {
    loop {
        let delivery = tokio::select! {
            () = stop.cancelled() => return,
            delivery = queue.next() => delivery,
        };
        let mut wait = FIRST_RETRY;
        for attempt in 1..=TRIES {
            let Some(client) = running_client(&host, &plugin, &stop).await else {
                return;
            };
            match post(&client, &delivery).await {
                Ok(()) => break,
                Err(detail) if attempt == TRIES => {
                    queue.lost();
                    host.log(
                        &plugin,
                        PluginLogStream::Host,
                        format!(
                            "event {} dropped after {TRIES} tries: {detail}",
                            delivery.seq
                        ),
                    );
                }
                Err(detail) => {
                    tracing::debug!(plugin = plugin.as_ref(), %detail, "plug-in event delivery retried");
                    tokio::select! {
                        () = stop.cancelled() => return,
                        () = tokio::time::sleep(wait) => {}
                    }
                    wait = (wait * 2).min(LAST_RETRY);
                }
            }
        }
    }
}

async fn running_client(
    host: &PluginHost,
    plugin: &PluginId,
    stop: &CancellationToken,
) -> Option<reqwest::Client> {
    loop {
        if let Some(client) = host.running_client(plugin) {
            return Some(client);
        }
        tokio::select! {
            () = stop.cancelled() => return None,
            () = tokio::time::sleep(RUNNING_POLL) => {}
        }
    }
}

async fn post(client: &reqwest::Client, delivery: &PluginEventDelivery) -> Result<(), String> {
    let response = client
        .post(super::plugin_url("events"))
        .timeout(ANSWER_WITHIN)
        .json(delivery)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("answered {}", response.status()))
    }
}

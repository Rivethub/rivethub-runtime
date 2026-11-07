use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use config::HubEntry;
use wire::gateway::HealthzResponse;
use wire::gateway::account::{Hub, HubKind};

use super::AccountError;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
const FRESH_FOR: Duration = Duration::from_secs(15);
pub const SERVING_HUB: &str = "this-node";

#[derive(Debug, Clone)]
struct Probe {
    online: bool,
    detail: String,
}

#[derive(Debug)]
pub struct Hubs {
    serving: Hub,
    entries: Vec<HubEntry>,
    http: reqwest::Client,
    probes: parking_lot::Mutex<BTreeMap<String, (Instant, Probe)>>,
}

impl Hubs {
    pub fn new(
        node: &str,
        serving_url: String,
        entries: Vec<HubEntry>,
        roots: Vec<reqwest::Certificate>,
    ) -> Result<Self, AccountError> {
        let mut builder = reqwest::Client::builder()
            .timeout(PROBE_TIMEOUT)
            .connect_timeout(PROBE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        Ok(Self {
            serving: Hub {
                id: SERVING_HUB.to_owned(),
                name: node.to_owned(),
                kind: HubKind::Local,
                url: serving_url,
                online: true,
                detail: "This node".to_owned(),
                current: true,
            },
            entries,
            http: builder.build()?,
            probes: parking_lot::Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn list(&self) -> Vec<Hub> {
        let answers =
            futures::future::join_all(self.entries.iter().map(|entry| self.probe(entry))).await;
        let mut hubs = vec![self.serving.clone()];
        hubs.extend(self.entries.iter().zip(answers).map(|(entry, probe)| Hub {
            id: hub_id(&entry.name),
            name: entry.name.clone(),
            kind: entry.kind,
            url: entry.url.to_string(),
            online: probe.online,
            detail: probe.detail,
            current: false,
        }));
        hubs
    }

    async fn probe(&self, entry: &HubEntry) -> Probe {
        let key = entry.url.to_string();
        if let Some((at, probe)) = self.probes.lock().get(&key)
            && at.elapsed() < FRESH_FOR
        {
            return probe.clone();
        }
        let probe = self.fetch(entry).await;
        self.probes
            .lock()
            .insert(key, (Instant::now(), probe.clone()));
        probe
    }

    async fn fetch(&self, entry: &HubEntry) -> Probe {
        let target = match url::Url::parse(&format!(
            "{}/healthz",
            entry.url.as_str().trim_end_matches('/')
        )) {
            Ok(target) => target,
            Err(error) => return offline(&error.to_string()),
        };
        let response = match self.http.get(target).send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() => return offline("did not answer in time"),
            Err(error) if error.is_connect() => return offline("unreachable"),
            Err(error) => return offline(&error.to_string()),
        };
        let status = response.status();
        let version = match response.json::<HealthzResponse>().await {
            Ok(health) => health.version,
            Err(error) => {
                tracing::debug!(%error, hub = %entry.name, "the hub health reply did not parse");
                None
            }
        };
        if status.is_success() {
            Probe {
                online: true,
                detail: match version {
                    Some(version) => format!("rh {version}"),
                    None => "Online".to_owned(),
                },
            }
        } else {
            offline(&format!("answered {status}"))
        }
    }
}

fn offline(reason: &str) -> Probe {
    Probe {
        online: false,
        detail: format!("Offline: {reason}"),
    }
}

fn hub_id(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_matches('-').to_owned()
}

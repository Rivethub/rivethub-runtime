use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use tokio_util::sync::CancellationToken;
use wire::gateway::account::{
    Account, AccountUpdate, Hub, PendingUpdate, SignInProgress, SignInStarted, UpdateSource,
};

use super::cloud::{Cloud, Delivery};
use super::hubs::Hubs;
use super::install::{AccountFiles, InstallState, Membership, UsageDay};
use super::telemetry::{CrashReport, UsageCounts, UsageReport, queued};
use super::update::{CloudOs, fetch, latest, plain_version};
use super::{AccountError, CloudSettings};

const FIRST_TICK: Duration = Duration::from_secs(20);
const TICK_EVERY: Duration = Duration::from_secs(600);
const CHECK_EVERY: Duration = Duration::from_hours(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageEvent {
    Search,
    AppOpen,
}

#[derive(Debug, Default)]
struct Counters {
    searches: AtomicU64,
    app_opens: AtomicU64,
}

#[derive(Debug)]
pub struct AccountService {
    files: AccountFiles,
    login: Option<Arc<dyn super::AccountLogin>>,
    settings: CloudSettings,
    cloud: Cloud,
    telemetry: Cloud,
    state: tokio::sync::Mutex<InstallState>,
    counters: Counters,
    hubs: Hubs,
    checked: parking_lot::Mutex<Option<Instant>>,
}

impl AccountService {
    pub async fn open(
        state_dir: &Path,
        settings: CloudSettings,
        hubs: Hubs,
    ) -> Result<Self, AccountError> {
        let files = AccountFiles::new(state_dir);
        let opening = files.clone();
        let state = tokio::task::spawn_blocking(move || {
            let mut state = opening.open()?;
            if !matches!(&state.membership, Membership::SignedOut) {
                state.membership = Membership::SignedOut;
                opening.save(&state)?;
            }
            Ok::<_, AccountError>(state)
        })
        .await??;
        Ok(Self {
            cloud: Cloud::new(settings.base.clone())?,
            telemetry: Cloud::new(settings.telemetry.clone())?,
            files,
            login: None,
            settings,
            state: tokio::sync::Mutex::new(state),
            counters: Counters::default(),
            hubs,
            checked: parking_lot::Mutex::new(None),
        })
    }

    #[must_use]
    pub fn files(&self) -> &AccountFiles {
        &self.files
    }

    pub async fn hubs(&self) -> Vec<Hub> {
        self.hubs.list().await
    }

    pub fn count(&self, event: UsageEvent) {
        let counter = match event {
            UsageEvent::Search => &self.counters.searches,
            UsageEvent::AppOpen => &self.counters.app_opens,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn with_login(mut self, login: Arc<dyn super::AccountLogin>) -> Self {
        self.login = Some(login);
        self
    }

    async fn identity(&self) -> Option<(String, String)> {
        if !self.settings.access.cloud() {
            return None;
        }
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B1 note 155: a cloud failure must show a signed-out account rather than preserve a cached claim of sign-in"
            )
        )]
        let identity = self.login.as_ref()?.identity().await.ok().flatten();
        identity
    }

    pub async fn account(&self) -> Account {
        let identity = self.identity().await;
        let state = self.state.lock().await;
        let mut account = self.view(&state);
        account.email = identity.map(|(_, email)| email);
        account.update_source = if account.email.is_some() {
            state.update_source.unwrap_or(UpdateSource::RivethubCloud)
        } else {
            UpdateSource::Github
        };
        drop(state);
        account
    }

    fn view(&self, state: &InstallState) -> Account {
        let allowed = self.settings.access.telemetry();
        let cloud = self.settings.access.cloud();
        Account {
            email: None,
            update_source: if cloud {
                state.update_source()
            } else {
                UpdateSource::Github
            },
            channel: state.channel,
            auto_download: state.auto_download,
            version: crate::VERSION.to_owned(),
            pending_update: state.pending_update.clone(),
            crash_reports: allowed && state.crash_reports,
            usage_counts: allowed && state.usage_counts,
            cloud_disabled: !cloud,
        }
    }

    pub async fn update(&self, change: AccountUpdate) -> Result<Account, AccountError> {
        let enabling = change.crash_reports == Some(true) || change.usage_counts == Some(true);
        if enabling && !self.settings.access.telemetry() {
            return Err(AccountError::Policy);
        }
        if change.update_source == Some(UpdateSource::RivethubCloud) {
            self.cloud_allowed()?;
        }
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        if let Some(source) = change.update_source {
            next.update_source = Some(source);
        }
        if let Some(channel) = change.channel {
            if channel != next.channel {
                next.pending_update = None;
            }
            next.channel = channel;
        }
        if let Some(auto_download) = change.auto_download {
            next.auto_download = auto_download;
        }
        if let Some(crash_reports) = change.crash_reports {
            next.crash_reports = crash_reports;
        }
        if let Some(usage_counts) = change.usage_counts {
            next.usage_counts = usage_counts;
            if !usage_counts {
                next.usage = None;
            }
        }
        self.save(&next).await?;
        *state = next;
        let account = self.view(&state);
        drop(state);
        if !account.crash_reports {
            self.forget_crashes().await?;
        }
        Ok(self.account().await)
    }

    fn cloud_allowed(&self) -> Result<(), AccountError> {
        if self.settings.access.cloud() {
            Ok(())
        } else {
            Err(AccountError::CloudPolicy)
        }
    }

    pub async fn sign_in(&self) -> Result<SignInStarted, AccountError> {
        self.cloud_allowed()?;
        let started = self
            .login
            .as_ref()
            .ok_or(AccountError::CloudReply)?
            .start()
            .await?;
        Ok(SignInStarted {
            user_code: started.user_code,
            verification_uri: started.verification_uri,
            expires_in: started.expires_in,
            poll_interval_ms: started.poll_interval_ms,
        })
    }

    pub async fn poll(&self) -> Result<SignInProgress, AccountError> {
        self.cloud_allowed()?;
        let progress = self
            .login
            .as_ref()
            .ok_or(AccountError::CloudReply)?
            .poll()
            .await?;
        Ok(match progress {
            super::LoginProgress::Pending => SignInProgress::Pending,
            super::LoginProgress::Expired => SignInProgress::Expired,
            super::LoginProgress::Denied => SignInProgress::Denied,
            super::LoginProgress::SignedIn => {
                *self.checked.lock() = None;
                let account = self.account().await;
                if account.email.is_none() {
                    return Err(AccountError::CloudReply);
                }
                SignInProgress::SignedIn { account }
            }
        })
    }

    pub async fn sign_out(&self) -> Result<Account, AccountError> {
        if let Some(login) = &self.login {
            login.sign_out(self.settings.access.cloud()).await?;
        }
        let mut state = self.state.lock().await;
        self.leave(&mut state).await
    }

    pub async fn delete(&self) -> Result<wire::gateway::account::AccountDeleted, AccountError> {
        self.cloud_allowed()?;
        Ok(wire::gateway::account::AccountDeleted {
            account: self.sign_out().await?,
            account_url: self.settings.base.join("account")?,
        })
    }

    async fn leave(&self, state: &mut InstallState) -> Result<Account, AccountError> {
        let mut next = state.clone();
        next.membership = Membership::SignedOut;
        next.pending_update = None;
        self.save(&next).await?;
        *state = next;
        Ok(self.view(state))
    }

    async fn save(&self, state: &InstallState) -> Result<(), AccountError> {
        let files = self.files.clone();
        let state = state.clone();
        tokio::task::spawn_blocking(move || files.save(&state)).await?
    }

    async fn forget_crashes(&self) -> Result<(), AccountError> {
        let files = self.files.clone();
        for path in tokio::task::spawn_blocking(move || queued(&files)).await?? {
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }

    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut beat =
            tokio::time::interval_at(tokio::time::Instant::now() + FIRST_TICK, TICK_EVERY);
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = beat.tick() => {}
            }
            self.tick(Utc::now(), &shutdown).await;
        }
    }

    pub async fn tick(&self, now: DateTime<Utc>, shutdown: &CancellationToken) {
        if let Err(error) = self.send_crashes().await {
            tracing::debug!(%error, "queued crash reports were not sent");
        }
        if let Err(error) = self.roll_usage(now.date_naive()).await {
            tracing::debug!(%error, "the usage report was not sent");
        }
        let due = self
            .checked
            .lock()
            .is_none_or(|checked| checked.elapsed() >= CHECK_EVERY);
        if due {
            *self.checked.lock() = Some(Instant::now());
            if let Err(error) = self.check_update(shutdown).await {
                tracing::warn!(%error, "the update check failed");
            }
        }
    }

    async fn send_crashes(&self) -> Result<(), AccountError> {
        let sending = self.settings.access.telemetry() && self.state.lock().await.crash_reports;
        let files = self.files.clone();
        for path in tokio::task::spawn_blocking(move || queued(&files)).await?? {
            if sending {
                let report: CrashReport = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
                match self.telemetry.crash(&report).await? {
                    Delivery::Accepted => {}
                    Delivery::Rejected => {
                        tracing::warn!(at = %report.at, "the telemetry endpoint rejected a crash report");
                    }
                }
            }
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }

    async fn roll_usage(&self, today: NaiveDate) -> Result<(), AccountError> {
        let searches = self.counters.searches.swap(0, Ordering::Relaxed);
        let app_opens = self.counters.app_opens.swap(0, Ordering::Relaxed);
        let mut state = self.state.lock().await;
        if !self.settings.access.telemetry() || !state.usage_counts {
            return Ok(());
        }
        let mut next = state.clone();
        let mut day = match next.usage.take() {
            Some(day) if day.day == today => day,
            Some(finished) => {
                self.report_usage(&state, &finished).await;
                UsageDay::starting(today)
            }
            None => UsageDay::starting(today),
        };
        day.searches = day.searches.saturating_add(searches);
        day.app_opens = day.app_opens.saturating_add(app_opens);
        next.usage = Some(day);
        self.save(&next).await?;
        *state = next;
        drop(state);
        Ok(())
    }

    async fn report_usage(&self, state: &InstallState, day: &UsageDay) {
        let Some(os) = CloudOs::current() else {
            return;
        };
        let report = UsageReport {
            schema: 1,
            install_id: state.install_id,
            rh: crate::VERSION.to_owned(),
            os,
            day: day.day,
            counts: UsageCounts {
                searches: Some(day.searches),
                app_opens: Some(day.app_opens),
            },
        };
        match self.telemetry.usage(&report).await {
            Ok(Delivery::Accepted) => {}
            Ok(Delivery::Rejected) => {
                tracing::warn!(day = %day.day, "the telemetry endpoint rejected the usage report");
            }
            Err(error) => tracing::debug!(%error, day = %day.day, "the usage report was not sent"),
        }
    }

    pub async fn check_update(&self, shutdown: &CancellationToken) -> Result<(), AccountError> {
        let snapshot = self.state.lock().await.clone();
        let pending = if snapshot.update_source == Some(UpdateSource::Github) {
            None
        } else if let Some((token, _)) = self.identity().await {
            match self.newer(&token, &snapshot, shutdown).await {
                Err(AccountError::SignedOutElsewhere) => {
                    self.sign_out().await?;
                    return Err(AccountError::SignedOutElsewhere);
                }
                result => result?,
            }
        } else {
            None
        };
        let mut state = self.state.lock().await;
        if state.pending_update != pending {
            let mut next = state.clone();
            next.pending_update = pending;
            self.save(&next).await?;
            *state = next;
        }
        drop(state);
        Ok(())
    }

    async fn newer(
        &self,
        token: &str,
        state: &InstallState,
        shutdown: &CancellationToken,
    ) -> Result<Option<PendingUpdate>, AccountError> {
        let Some((manifest, digest)) =
            latest(&self.cloud, &self.settings, token, state.channel).await?
        else {
            return Ok(None);
        };
        let running = plain_version(crate::VERSION)?;
        if plain_version(&manifest.version)? <= running {
            return Ok(None);
        }
        let pending = PendingUpdate {
            version: manifest.version.clone(),
            notes_url: manifest.notes_url.clone(),
        };
        if state.auto_download {
            fetch(&self.cloud, &self.files, manifest, digest, shutdown).await?;
        }
        Ok(Some(pending))
    }
}

impl UsageDay {
    fn starting(day: NaiveDate) -> Self {
        Self {
            day,
            searches: 0,
            app_opens: 0,
        }
    }
}

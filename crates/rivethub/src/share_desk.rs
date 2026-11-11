use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use capture::bundle::Sealed;
use capture::sanitize::LocalKey;
use capture::sanitize::LocalReview;
use capture::sanitize::ReportFlags;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Resolution;
use capture::share;
use chrono::DateTime;
use chrono::Utc;
use cli::memory::SessionStore;
use cli::memory::desk as cli_desk;
use config::Config;
use config::Environment;
use kernel::memory::MemoryStore;
use kernel::memory::share::DecideMode;
use kernel::memory::share::SessionShare as _;
use kernel::memory::share::ShareFailure;
use kernel::memory::share::desk::DRAFT_IDLE;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::Draft;
use kernel::memory::share::desk::DraftDecision;
use kernel::memory::share::desk::DraftId;
use kernel::memory::share::desk::DraftRequest;
use kernel::memory::share::desk::DraftSession;
use kernel::memory::share::desk::DraftTotals;
use kernel::memory::share::desk::EscapedText;
use kernel::memory::share::desk::FindQuery;
use kernel::memory::share::desk::FlagChoice;
use kernel::memory::share::desk::FlagState;
use kernel::memory::share::desk::FlagView;
use kernel::memory::share::desk::Found;
use kernel::memory::share::desk::FoundSession;
use kernel::memory::share::desk::LoginId;
use kernel::memory::share::desk::LoginRequest;
use kernel::memory::share::desk::LoginStep;
use kernel::memory::share::desk::MAX_DRAFT_BYTES;
use kernel::memory::share::desk::MAX_DRAFTS;
use kernel::memory::share::desk::Mark;
use kernel::memory::share::desk::Marked;
use kernel::memory::share::desk::NodeKeyView;
use kernel::memory::share::desk::RuleCount;
use kernel::memory::share::desk::SendRequest;
use kernel::memory::share::desk::Sent;
use kernel::memory::share::desk::ServerOrigin;
use kernel::memory::share::desk::Servers;
use kernel::memory::share::desk::ShareDesk;
use kernel::memory::share::find;
use kernel::owner::Owner;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::sessions::NodeShare;

const DRAFT_LABEL: &str = "the draft bundle";
const CLIENT: &str = "RivetHub desktop";
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct NodeDesk {
    share: NodeShare,
    env: Environment,
    memory: Arc<dyn MemoryStore>,
    clock: Clock,
    drafts: parking_lot::Mutex<BTreeMap<DraftId, Slot>>,
    logins: parking_lot::Mutex<BTreeMap<LoginId, Pending>>,
}

struct Slot {
    bytes: usize,
    touched: DateTime<Utc>,
    state: Arc<Mutex<DraftState>>,
}

struct DraftState {
    sealed: Sealed,
    local: Option<LocalReview>,
    report: capture::sanitize::Report,
    resolved: BTreeMap<usize, Resolution>,
    conversations: Vec<Uuid>,
}

struct Pending {
    expires_at: DateTime<Utc>,
    login: Arc<Mutex<cli_desk::PendingLogin>>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: drafts hold flagged text; Debug shows counts only"
    )
)]
impl std::fmt::Debug for NodeDesk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeDesk")
            .field("drafts", &self.drafts.lock().len())
            .field("logins", &self.logins.lock().len())
            .finish_non_exhaustive()
    }
}

impl NodeDesk {
    pub fn new<E>(
        config: &Config,
        env: &Environment,
        owner: Owner,
        store: SessionStore,
        memory: Arc<dyn MemoryStore>,
    ) -> Result<Self, E>
    where
        E: From<config::ConfigError> + From<crate::NodeError>,
    {
        Ok(Self {
            share: NodeShare::new::<E>(config, env, owner, store)?,
            env: env.clone(),
            memory,
            clock: Arc::new(Utc::now),
            drafts: parking_lot::Mutex::default(),
            logins: parking_lot::Mutex::default(),
        })
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub async fn sweep_until(self: Arc<Self>, shutdown: CancellationToken) {
        let mut every = tokio::time::interval(SWEEP_EVERY);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = every.tick() => self.sweep(),
            }
        }
    }

    pub fn sweep(&self) {
        let now = (self.clock)();
        self.drafts
            .lock()
            .retain(|_, slot| slot.touched + DRAFT_IDLE > now);
        self.logins
            .lock()
            .retain(|_, pending| pending.expires_at > now);
    }

    fn slot(&self, id: DraftId) -> Result<Arc<Mutex<DraftState>>, DeskError> {
        self.sweep();
        let now = (self.clock)();
        self.drafts
            .lock()
            .get_mut(&id)
            .map(|slot| {
                slot.touched = now;
                Arc::clone(&slot.state)
            })
            .ok_or(DeskError::UnknownDraft)
    }

    fn resized(&self, id: DraftId, bytes: usize) -> DateTime<Utc> {
        let now = (self.clock)();
        if let Some(slot) = self.drafts.lock().get_mut(&id) {
            slot.bytes = bytes;
            slot.touched = now;
        }
        now + DRAFT_IDLE
    }

    fn admit(&self) -> Result<(), DeskError> {
        self.sweep();
        if self.held(0) {
            return Err(DeskError::Full);
        }
        Ok(())
    }

    fn held(&self, adding: usize) -> bool {
        let drafts = self.drafts.lock();
        let bytes: usize = drafts.values().map(|slot| slot.bytes).sum();
        drafts.len() >= MAX_DRAFTS || bytes + adding > MAX_DRAFT_BYTES
    }
}

fn view(id: DraftId, state: &DraftState, expires_at: DateTime<Utc>) -> Draft {
    let report = &state.report;
    let totals = report.totals();
    let decision = report.decision.as_ref();
    Draft {
        id,
        expires_at,
        totals: DraftTotals {
            secrets: totals.secrets,
            mapped: totals.mapped,
            redacted: totals.redacted,
            escaped: totals.escaped,
            rechecked: report.rechecked,
        },
        rules: share::rule_totals(report)
            .into_iter()
            .map(|(rule, count)| RuleCount { rule, count })
            .collect(),
        sessions: state
            .sealed
            .bundle
            .sessions
            .iter()
            .zip(&state.conversations)
            .map(|(session, id)| DraftSession {
                id: *id,
                agent: session.origin.agent.clone(),
                messages: session.batch.messages.len(),
            })
            .collect(),
        not_scanned: share::NOT_SCANNED.to_owned(),
        best_effort: share::BEST_EFFORT.to_owned(),
        decision: decision.map(|decision| DraftDecision {
            model: decision.model.clone(),
            scored: decision.scored,
            calls: decision.calls,
            trivial: decision.trivial,
            mode: match decision.review.mode {
                Mode::Advisory => DecideMode::Advisory,
                Mode::Block => DecideMode::Block,
            },
        }),
        flags: decision
            .map(|decision| {
                decision
                    .flags
                    .iter()
                    .map(|flag| {
                        let place = crate::sessions::flag_place(flag);
                        FlagView {
                            number: flag.number,
                            score: flag.score,
                            session: place
                                .as_ref()
                                .and_then(|place| state.conversations.get(place.session))
                                .copied(),
                            place,
                            length: flag.text.chars().count(),
                            text: EscapedText::escape(&flag.text),
                            state: flag_state(state.resolved.get(&flag.number)),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

impl NodeDesk {
    async fn learned(&self, marks: &[Mark]) -> Result<Option<String>, DeskError> {
        let maps: Vec<(&str, &str)> = marks
            .iter()
            .filter(|mark| mark.learn)
            .filter_map(|mark| match &mark.choice {
                FlagChoice::Map { term, placeholder } => {
                    Some((term.as_str(), placeholder.as_str()))
                }
                FlagChoice::Keep | FlagChoice::Redact => None,
            })
            .collect();
        if maps.is_empty() {
            return Ok(None);
        }
        let offered = cli_desk::offer(&self.share.mapping, &maps, true)
            .await
            .map_err(cli_desk::desk_error)?;
        if let Some((path, text)) = offered.write {
            share::write_file(path, text.into_bytes(), share::Clobber::Replace)
                .await
                .map_err(failed)?;
        }
        Ok(Some(offered.message))
    }
}

fn flag_state(resolution: Option<&Resolution>) -> FlagState {
    match resolution {
        None => FlagState::Open,
        Some(Resolution::Keep) => FlagState::Keep,
        Some(Resolution::Redact) => FlagState::Redact,
        Some(Resolution::Map { term, placeholder }) => FlagState::Map {
            term: term.clone(),
            placeholder: placeholder.clone(),
        },
    }
}

fn failed(error: impl std::fmt::Display) -> DeskError {
    DeskError::Failed(error.to_string())
}

fn share_failure(error: ShareFailure) -> DeskError {
    match error {
        ShareFailure::DecideUnconfigured => DeskError::DecideUnconfigured,
        ShareFailure::UnknownSessions(ids) => DeskError::UnknownSessions(ids),
        other @ (ShareFailure::Exists(_)
        | ShareFailure::NotBundle(_)
        | ShareFailure::Failed(_)
        | ShareFailure::Memory(_)
        | ShareFailure::Io(_)) => failed(other),
    }
}

fn with_newline(text: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(text.len() + 1);
    line.extend_from_slice(text);
    line.push(b'\n');
    line
}

#[async_trait]
impl ShareDesk for NodeDesk {
    async fn find(&self, query: FindQuery) -> Result<Found, DeskError> {
        let text = query.query.trim().to_owned();
        if text.is_empty() {
            return Err(DeskError::Choices(
                "query must name what the sessions were about".to_owned(),
            ));
        }
        let wanted = query.limit.unwrap_or(find::DEFAULT_SESSIONS);
        if !(1..=find::MAX_SESSIONS).contains(&wanted) {
            return Err(DeskError::Choices(format!(
                "limit is {wanted}; choose from 1 to {}",
                find::MAX_SESSIONS
            )));
        }
        let tag = query.tag.map(|tag| tag.parse()).transpose().map_err(
            |error: kernel::memory::TagLiteralError| DeskError::Choices(error.to_string()),
        )?;
        let outcome = self
            .memory
            .search(find::query(
                find::Search {
                    text,
                    agent: query.agent,
                    tag,
                    since: query.since,
                    before: query.before,
                },
                wanted,
            ))
            .await
            .map_err(failed)?;
        let ranked = find::group(&outcome.results, |text| self.share.redact(text));
        let keys: Vec<String> = ranked.iter().map(|session| session.key.clone()).collect();
        let digests = if keys.is_empty() {
            Vec::new()
        } else {
            self.share.digests(&keys).await.map_err(share_failure)?
        };
        let matched = find::matched(&ranked, &digests, wanted);
        Ok(Found {
            sessions: matched
                .found
                .into_iter()
                .map(|(ranked, digest)| FoundSession {
                    id: digest.conversation_id,
                    title: digest
                        .title
                        .as_deref()
                        .map(|title| self.share.redact(title)),
                    agent: digest.agent.clone(),
                    first: digest.first,
                    last: digest.last,
                    messages: digest.messages,
                    best: ranked.best,
                    hits: ranked.hits,
                    excerpts: ranked.excerpts.clone(),
                    imported_from: digest.imported_from.clone(),
                })
                .collect(),
            dropped: matched.dropped,
        })
    }

    async fn draft(&self, request: DraftRequest) -> Result<Draft, DeskError> {
        self.admit()?;
        let decider = self
            .share
            .decider(request.decide, request.block)
            .map_err(share_failure)?;
        let sessions = self
            .share
            .store
            .chosen(&self.share.author, &self.share.owner, &request.sessions)
            .await
            .map_err(share_failure)?;
        let conversations = sessions
            .iter()
            .map(|(session, _)| session.origin.conversation_id.parse::<Uuid>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(failed)?;
        let chosen =
            share::chosen(self.share.author.clone(), sessions, Utc::now()).map_err(failed)?;
        let outcome = share::sanitize_in_memory(
            share::Job {
                bundle: &chosen.bundle,
                sessions: &chosen.sessions,
                mapping: &self.share.mapping,
                key: &self.share.key,
                decider: decider.as_ref(),
                at: Utc::now(),
            },
            DRAFT_LABEL,
        )
        .await
        .map_err(failed)?;
        let bytes = outcome.sanitized.text.len();
        let state = DraftState {
            sealed: outcome.sanitized,
            local: outcome.local,
            report: outcome.report,
            resolved: BTreeMap::new(),
            conversations,
        };
        let id = DraftId::random();
        let now = (self.clock)();
        let draft = view(id, &state, now + DRAFT_IDLE);
        self.sweep();
        if self.held(bytes) {
            return Err(DeskError::Full);
        }
        self.drafts.lock().insert(
            id,
            Slot {
                bytes,
                touched: now,
                state: Arc::new(Mutex::new(state)),
            },
        );
        Ok(draft)
    }

    async fn report(&self, id: DraftId) -> Result<Draft, DeskError> {
        let slot = self.slot(id)?;
        let state = slot.lock().await;
        let expires_at = self.resized(id, state.sealed.text.len());
        let draft = view(id, &state, expires_at);
        drop(state);
        Ok(draft)
    }

    async fn mark(&self, id: DraftId, marks: Vec<Mark>) -> Result<Marked, DeskError> {
        let slot = self.slot(id)?;
        let mut state = slot.lock().await;
        let local = state.local.as_ref().ok_or_else(|| {
            DeskError::Marks("the draft was made without decide, so it has no flags".to_owned())
        })?;
        let marks_error = |error: &dyn std::fmt::Display| DeskError::Marks(error.to_string());
        let flags = ReportFlags::from_choices(local, &state.resolved, &marks)
            .map_err(|error| marks_error(&error))?;
        for flag in &flags.flags {
            if marks
                .iter()
                .any(|mark| mark.learn && mark.number == flag.number)
            {
                flag.learnable().map_err(|error| marks_error(&error))?;
            }
        }
        let key = LocalKey::load_or_create(&self.share.key).map_err(failed)?;
        let resolved = capture::sanitize::resolve(&state.sealed.bundle, &flags, local, &key)
            .map_err(|error| marks_error(&error))?;
        let rechecked: Vec<String> = resolved.rechecked.iter().map(ToString::to_string).collect();
        state.report.rechecked += resolved.rechecked.len();
        state.report.changes.extend(resolved.rechecked);
        state.resolved.extend(resolved.applied);
        state.sealed = resolved.sealed;
        let expires_at = self.resized(id, state.sealed.text.len());
        let draft = view(id, &state, expires_at);
        drop(state);
        Ok(Marked {
            draft,
            rechecked,
            learned: self.learned(&marks).await?,
        })
    }

    async fn send(&self, id: DraftId, send: SendRequest) -> Result<Sent, DeskError> {
        let slot = self.slot(id)?;
        let state = slot.lock().await;
        let flags = state
            .report
            .decision
            .as_ref()
            .map_or(0, |decision| decision.flags.len());
        let open = flags.saturating_sub(state.resolved.len());
        if open > 0 {
            return Err(DeskError::OpenFlags { open });
        }
        let sessions = state.sealed.bundle.sessions.len();
        let kept = state
            .resolved
            .values()
            .filter(|resolution| **resolution == Resolution::Keep)
            .count();
        if send.confirm.sessions != sessions || send.confirm.kept_flags != kept {
            return Err(DeskError::Counts {
                sessions,
                kept,
                confirmed_sessions: send.confirm.sessions,
                confirmed_kept: send.confirm.kept_flags,
            });
        }
        let sealed =
            cli_desk::seal_bytes(&self.env, with_newline(&state.sealed.text), send.signed, 0)
                .await
                .map_err(cli_desk::desk_error)?;
        let shared = cli_desk::upload_to(&self.env, Some(send.server.as_str()), sealed)
            .await
            .map_err(cli_desk::desk_error)?;
        drop(state);
        self.drafts.lock().remove(&id);
        Ok(Sent {
            code: shared.code,
            expires_at: shared.expires_at,
            bytes: shared.uploaded,
            signed: shared.signed.map(|key| key.fingerprint()),
        })
    }

    async fn discard(&self, id: DraftId) -> Result<(), DeskError> {
        self.sweep();
        self.drafts
            .lock()
            .remove(&id)
            .map(|_| ())
            .ok_or(DeskError::UnknownDraft)
    }

    async fn servers(&self) -> Result<Servers, DeskError> {
        cli_desk::servers(&self.env)
            .await
            .map_err(cli_desk::desk_error)
    }

    async fn login(&self, request: LoginRequest) -> Result<LoginStep, DeskError> {
        match request {
            LoginRequest::Cloud { origin } => {
                let started = cli_desk::start(&self.env, Some(origin.as_str()), CLIENT)
                    .await
                    .map_err(cli_desk::desk_error)?;
                let login = LoginId::random();
                let interval = started.pending.interval().as_secs();
                self.sweep();
                self.logins.lock().insert(
                    login,
                    Pending {
                        expires_at: started.expires_at,
                        login: Arc::new(Mutex::new(started.pending)),
                    },
                );
                Ok(LoginStep::Started {
                    login,
                    user_code: started.user_code,
                    verification_uri_complete: started.verification_uri_complete,
                    interval,
                    expires_at: started.expires_at,
                })
            }
            LoginRequest::Team { origin, token } => {
                let user = cli_desk::login_team(&self.env, origin.as_str(), token)
                    .await
                    .map_err(cli_desk::desk_error)?;
                Ok(LoginStep::SignedIn {
                    origin,
                    user: Some(user),
                })
            }
        }
    }

    async fn poll(&self, login: LoginId) -> Result<LoginStep, DeskError> {
        self.sweep();
        let pending = self
            .logins
            .lock()
            .get(&login)
            .map(|pending| Arc::clone(&pending.login))
            .ok_or(DeskError::UnknownLogin)?;
        let mut pending = pending.lock().await;
        let polled = cli_desk::poll(&self.env, &mut pending)
            .await
            .map_err(cli_desk::desk_error)?;
        let (interval, origin) = (
            pending.interval().as_secs(),
            ServerOrigin::from(pending.server().to_string()),
        );
        drop(pending);
        let step = match polled {
            cli_desk::Polled::Pending | cli_desk::Polled::SlowDown { .. } => {
                return Ok(LoginStep::Pending { interval });
            }
            cli_desk::Polled::SignedIn { .. } => Ok(LoginStep::SignedIn { origin, user: None }),
            cli_desk::Polled::Denied => Err(DeskError::LoginDenied("denied in the browser")),
            cli_desk::Polled::Expired => {
                Err(DeskError::LoginDenied("not given before the code expired"))
            }
        };
        self.logins.lock().remove(&login);
        step
    }

    async fn logout(&self, origin: ServerOrigin) -> Result<(), DeskError> {
        cli_desk::logout(&self.env, Some(origin.as_str()))
            .await
            .map(|_| ())
            .map_err(cli_desk::desk_error)
    }

    async fn key(&self) -> Result<NodeKeyView, DeskError> {
        let key = cli_desk::node_key(&self.env).map_err(cli_desk::desk_error)?;
        Ok(NodeKeyView {
            key: key.to_string(),
            fingerprint: key.fingerprint(),
        })
    }
}

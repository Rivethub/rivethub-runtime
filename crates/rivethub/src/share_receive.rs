use std::str::FromStr as _;
use std::sync::Arc;

use async_trait::async_trait;
use capture::receive::Blocked;
use capture::receive::Checked;
use capture::receive::Decision;
use capture::receive::Decisions;
use capture::receive::Family;
use capture::receive::Row;
use capture::receive::visible;
use capture::redaction::Redaction;
use capture::seal::ShareCode;
use capture::share_policy::ShareRefused;
use capture::sign::PublicKey;
use capture::sign::TrustError;
use capture::sign::Verification as Signing;
use chrono::Utc;
use cli::memory::FetchError;
use cli::memory::Fetched;
use cli::share_server::ServerError;
use config::Config;
use config::Environment;
use kernel::memory::MemoryStore;
use kernel::memory::share::receive as port;
use kernel::memory::share::receive::ReceiveError;
use kernel::owner::Owner;
use wire::capture::ChecksDecision;
use wire::capture::ReceiptVerification;

const TITLE_CHARS: usize = 120;
const MESSAGE_CHARS: usize = 300;

pub struct NodeReceiver {
    env: Environment,
    owner: Owner,
    node: String,
    store: Arc<dyn MemoryStore>,
    redaction: Option<Redaction>,
    busy: Arc<tokio::sync::Mutex<()>>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "the MemoryStore port is not Debug, so the store field cannot be derived"
    )
)]
impl std::fmt::Debug for NodeReceiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeReceiver")
            .field("owner", &self.owner)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl NodeReceiver {
    #[must_use]
    pub fn new(
        config: &Config,
        env: &Environment,
        owner: Owner,
        node: String,
        store: Arc<dyn MemoryStore>,
    ) -> Self {
        Self {
            env: env.clone(),
            owner,
            node,
            store,
            redaction: capture::redaction::resolve(Some(config.capture_redaction()), env),
            busy: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn open(
        &self,
        code: port::ReceiveCode,
    ) -> Result<(Fetched, tokio::sync::OwnedMutexGuard<()>), ReceiveError> {
        let code = ShareCode::from_str(code.reveal())
            .map_err(|error| ReceiveError::BadCode(error.to_string()))?;
        let guard = Arc::clone(&self.busy).lock_owned().await;
        let env = self.env.clone();
        let task = tokio::spawn(async move {
            let fetched = cli::memory::fetch(&env, code, |_| Ok(())).await;
            (fetched, guard)
        });
        let (fetched, guard) = task
            .await
            .map_err(|error| ReceiveError::Failed(error.to_string()))?;
        Ok((fetched.map_err(failed)?, guard))
    }
}

#[async_trait]
impl port::ReceiveDesk for NodeReceiver {
    async fn inspect(&self, code: port::ReceiveCode) -> Result<port::Inspected, ReceiveError> {
        let (fetched, _one_at_a_time) = self.open(code).await?;
        let (fetched, checked) = screened(fetched).await?;
        Ok(inspected(&fetched, &checked))
    }

    async fn import(
        &self,
        code: port::ReceiveCode,
        request: port::ImportRequest,
    ) -> Result<port::Imported, ReceiveError> {
        let (fetched, _one_at_a_time) = self.open(code).await?;
        let (fetched, checked) = screened(fetched).await?;
        let bundle = fetched.bundle();
        let count = bundle.sessions.len();
        let mut sessions = std::collections::BTreeMap::new();
        for choice in &request.sessions {
            let index = choice
                .number
                .checked_sub(1)
                .filter(|index| *index < count)
                .ok_or_else(|| {
                    ReceiveError::NoSuchSession(format!(
                        "the bundle has no session {}; it holds {count}",
                        choice.number
                    ))
                })?;
            sessions.insert(index, decision(choice.choice));
        }
        let decisions = Decisions {
            all: None,
            sessions,
        };
        let plan = checked
            .plan(bundle, &decisions, request.force)
            .map_err(blocked)?;
        let accepts_findings = request.force
            || plan
                .imports
                .iter()
                .any(|accepted| accepted.checks.decision == ChecksDecision::Import);
        if accepts_findings && !request.fresh_login {
            return Err(ReceiveError::FreshLoginRequired);
        }
        let now = Utc::now();
        let receipt = fetched.receipt(now);
        let batches = received_batches(&plan, &bundle.manifest.exported_by.user, &receipt)?;
        let mut imported = Vec::with_capacity(batches.len());
        for (index, checks, verification, batch) in &batches {
            imported.push(self.write(*index, *checks, *verification, batch).await?);
        }
        fetched
            .mark_received()
            .map_err(|error| ReceiveError::Failed(error.to_string()))?;
        Ok(port::Imported {
            bundle: fetched.id.to_string(),
            received_at: now,
            author: visible(&bundle.manifest.exported_by.user),
            verification: verification(receipt.verification),
            signer: signer(&fetched.opened.verification),
            sessions: imported,
            skipped: plan.skipped.iter().map(|index| index + 1).collect(),
        })
    }

    async fn trusted(&self) -> Result<Vec<port::Sender>, ReceiveError> {
        let senders = cli::memory::trusted_senders(&self.env).map_err(failed)?;
        Ok(senders.iter().map(sender).collect())
    }

    async fn trust(&self, name: String, key: String) -> Result<port::Sender, ReceiveError> {
        let key =
            PublicKey::from_str(&key).map_err(|error| ReceiveError::BadKey(error.to_string()))?;
        let _one_at_a_time = self.busy.lock().await;
        cli::memory::trust_sender(&self.env, &name, key)
            .map(|added| sender(&added))
            .map_err(failed)
    }

    async fn untrust(&self, name: String, key: Option<String>) -> Result<usize, ReceiveError> {
        let key = key
            .map(|text| PublicKey::from_str(&text))
            .transpose()
            .map_err(|error| ReceiveError::BadKey(error.to_string()))?;
        let _one_at_a_time = self.busy.lock().await;
        cli::memory::untrust_sender(&self.env, &name, key.as_ref()).map_err(failed)
    }
}

impl NodeReceiver {
    async fn write(
        &self,
        index: usize,
        checks: ChecksDecision,
        verification: ReceiptVerification,
        batch: &wire::capture::CaptureBatchRequest,
    ) -> Result<port::ImportedSession, ReceiveError> {
        let prepared = capture::writer::prepare(batch, self.redaction.as_ref())
            .map_err(|error| ReceiveError::Failed(error.to_string()))?;
        let key = wire::capture::owned_import_key(self.owner.as_str(), &prepared.session_key)
            .map_err(|error| ReceiveError::Failed(error.to_string()))?
            .ok_or_else(|| ReceiveError::Failed("a received batch has no import key".to_owned()))?;
        let captured = self
            .store
            .capture(
                crate::sessions::kernel_batch(prepared),
                &self.owner,
                Some(&self.node),
            )
            .await
            .map_err(|error| ReceiveError::Failed(error.to_string()))?;
        Ok(port::ImportedSession {
            number: index + 1,
            key,
            verification: self::verification(verification),
            verdict: match checks {
                ChecksDecision::Clean => port::Verdict::Clean,
                ChecksDecision::Import => port::Verdict::Import,
                ChecksDecision::HoldSpans => port::Verdict::HoldSpans,
            },
            inserted: captured.inserted,
            skipped: captured.skipped,
        })
    }
}

type Received = (
    usize,
    ChecksDecision,
    ReceiptVerification,
    wire::capture::CaptureBatchRequest,
);

fn received_batches(
    plan: &capture::receive::Plan<'_>,
    exporter: &str,
    receipt: &wire::capture::Receipt,
) -> Result<Vec<Received>, ReceiveError> {
    let mut batches = Vec::with_capacity(plan.imports.len());
    for accepted in &plan.imports {
        let forwarded = accepted
            .session
            .origin
            .known_author()
            .is_some_and(|author| author != exporter);
        let verification = match receipt.verification {
            ReceiptVerification::Verified if forwarded => ReceiptVerification::UnknownKey,
            kept @ (ReceiptVerification::Verified
            | ReceiptVerification::Unsigned
            | ReceiptVerification::UnknownKey) => kept,
        };
        let batch = accepted
            .session
            .received_batch(&wire::capture::Receipt {
                checks: Some(accepted.checks.clone()),
                verification,
                ..receipt.clone()
            })
            .map_err(|error| ReceiveError::Failed(error.to_string()))?;
        check_batch(accepted.index, &batch)?;
        batches.push((
            accepted.index,
            accepted.checks.decision,
            verification,
            batch,
        ));
    }
    Ok(batches)
}

async fn screened(fetched: Fetched) -> Result<(Fetched, Checked), ReceiveError> {
    tokio::task::spawn_blocking(move || {
        let checked = capture::receive::check(fetched.bundle());
        (fetched, checked)
    })
    .await
    .map_err(|error| ReceiveError::Failed(error.to_string()))
}

fn check_batch(
    index: usize,
    batch: &wire::capture::CaptureBatchRequest,
) -> Result<(), ReceiveError> {
    let blank = batch.agent.trim().is_empty()
        || batch
            .messages
            .iter()
            .any(|message| message.event_id.is_empty());
    if blank {
        return Err(ReceiveError::Unreadable(format!(
            "session {} has a blank agent or an empty event id, so nothing was imported",
            index + 1
        )));
    }
    Ok(())
}

fn inspected(fetched: &Fetched, checked: &Checked) -> port::Inspected {
    let bundle = fetched.bundle();
    let author = &bundle.manifest.exported_by;
    let verification = &fetched.opened.verification;
    let by_author = || {
        checked
            .findings()
            .iter()
            .filter(|finding| finding.session.is_none())
    };
    port::Inspected {
        bundle: fetched.id.to_string(),
        server: fetched.server.to_string(),
        downloaded: fetched.downloaded,
        author: visible(&author.user),
        node: visible(&author.node),
        verification: self::verification(verification.kind()),
        reason: verification.reason(&visible(&author.user)),
        signer: signer(verification),
        sessions: (0..bundle.sessions.len())
            .filter_map(|index| inbound(checked, bundle, index))
            .collect(),
        author_findings: by_author().count(),
        author_rows: rows(checked.manifest_rows()),
        summary: checked.summary(),
    }
}

fn inbound(
    checked: &Checked,
    bundle: &wire::capture::SessionBundle,
    index: usize,
) -> Option<port::Inbound> {
    let shown = checked.shown(bundle, index)?;
    let found = || {
        checked
            .findings()
            .iter()
            .filter(move |finding| finding.session == Some(index))
    };
    Some(port::Inbound {
        number: index + 1,
        author: shown.origin.known_author().map(visible),
        title: shown.batch.title.as_deref().map_or_else(
            || "untitled".to_owned(),
            |title| visible(title).chars().take(TITLE_CHARS).collect(),
        ),
        agent: visible(&shown.origin.agent),
        messages: shown.batch.messages.len(),
        created_at: shown.origin.created_at,
        updated_at: shown.origin.updated_at,
        findings: found().count(),
        secret: found().any(|finding| finding.family == Family::Secret),
        rows: rows(checked.session_rows(index)),
    })
}

fn rows(found: Vec<Row>) -> Vec<port::FindingRow> {
    found
        .into_iter()
        .take(capture::receive::ROWS_PER_SESSION)
        .map(|row| port::FindingRow {
            event: row.event,
            field: row.field,
            rule: row.rule.to_owned(),
            family: match row.family {
                Family::Secret => port::FindingFamily::Secret,
                Family::Injection(_) => port::FindingFamily::Injection,
            },
            kind: row.family.to_string(),
            count: row.count,
            excerpt: row.excerpt,
        })
        .collect()
}

const fn decision(choice: port::Choice) -> Decision {
    match choice {
        port::Choice::Import => Decision::Import,
        port::Choice::Skip => Decision::Skip,
        port::Choice::HoldSpans => Decision::HoldSpans,
    }
}

const fn verification(kind: ReceiptVerification) -> port::Verification {
    match kind {
        ReceiptVerification::Verified => port::Verification::Verified,
        ReceiptVerification::Unsigned => port::Verification::Unsigned,
        ReceiptVerification::UnknownKey => port::Verification::UnknownKey,
    }
}

fn signer(verification: &Signing) -> Option<port::Signer> {
    verification.key().map(|key| port::Signer {
        key: key.to_string(),
        fingerprint: key.fingerprint(),
    })
}

fn sender(trusted: &capture::sign::TrustedSender) -> port::Sender {
    port::Sender {
        name: trusted.name.clone(),
        key: trusted.key.to_string(),
        fingerprint: trusted.key.fingerprint(),
    }
}

fn blocked(error: Blocked) -> ReceiveError {
    let numbered = |sessions: Vec<usize>| sessions.into_iter().map(|index| index + 1).collect();
    match error {
        Blocked::NoSuchSession { .. } => ReceiveError::NoSuchSession(error.to_string()),
        Blocked::Manifest => ReceiveError::AuthorFindings,
        Blocked::Undecided { sessions } => ReceiveError::Undecided(numbered(sessions)),
        Blocked::SecretImport { sessions } => ReceiveError::SecretImport(numbered(sessions)),
    }
}

fn failed(error: FetchError) -> ReceiveError {
    use capture::seal::OpenError;
    use cli::memory::DropError;
    match error {
        FetchError::AlreadyReceived(id) => ReceiveError::AlreadyReceived(id.to_string()),
        FetchError::Drop(error) => match error {
            DropError::Gone(_) => ReceiveError::Gone(error.to_string()),
            DropError::Burned(_) | DropError::FailedMidway(_) => {
                ReceiveError::Burned(error.to_string())
            }
            DropError::TooLarge => ReceiveError::TooLarge,
            DropError::Server(error) => server(&error),
            DropError::BadId(_) | DropError::Io(_) => ReceiveError::Failed(error.to_string()),
        },
        FetchError::Server(error) => server(&error),
        FetchError::Open(OpenError::Decrypt) => ReceiveError::Undecryptable,
        FetchError::Open(OpenError::Signature(error)) => {
            ReceiveError::BadSignature(shown(&error.to_string()))
        }
        FetchError::Open(error) => ReceiveError::Unreadable(shown(&error.to_string())),
        FetchError::Trust(error @ TrustError::BlankName) => {
            ReceiveError::BadName(error.to_string())
        }
        FetchError::Trust(error @ TrustError::Duplicate { .. }) => {
            ReceiveError::Duplicate(error.to_string())
        }
        FetchError::Share(cli::memory::ShareError::Raw) => ReceiveError::Raw,
        FetchError::Share(cli::memory::ShareError::Withheld(count)) => {
            ReceiveError::Withheld(count)
        }
        error @ (FetchError::Setup(_)
        | FetchError::Trust(_)
        | FetchError::Share(_)
        | FetchError::Config(_)
        | FetchError::Io(_)) => ReceiveError::Failed(error.to_string()),
    }
}

fn shown(text: &str) -> String {
    visible(text).chars().take(MESSAGE_CHARS).collect()
}

fn server(error: &ServerError) -> ReceiveError {
    match error {
        ServerError::Server(code) => ReceiveError::BadCode(code.to_string()),
        ServerError::Policy(ShareRefused::Unlisted { .. }) => ReceiveError::Refused(
            "this hub's policy does not allow that share server (memory.share.allowed_servers)"
                .to_owned(),
        ),
        ServerError::Http(_)
        | ServerError::Answered(_)
        | ServerError::Unavailable { .. }
        | ServerError::Protocol(_)
        | ServerError::Lifetime(_) => ReceiveError::Unreachable(shown(&error.to_string())),
        ServerError::AllowedServer(_)
        | ServerError::Policy(_)
        | ServerError::LoggedOut(_)
        | ServerError::Expired { .. }
        | ServerError::Denied(_)
        | ServerError::Refused(_)
        | ServerError::TokenRefused { .. }
        | ServerError::Keyring(_)
        | ServerError::Finished
        | ServerError::NoDeviceFlow(_)
        | ServerError::Store(_)
        | ServerError::Ca { .. }
        | ServerError::File(_)
        | ServerError::Io(_)
        | ServerError::Cancelled => ReceiveError::Refused(shown(&error.to_string())),
    }
}

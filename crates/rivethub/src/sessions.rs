use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use capture::redaction::Redaction;
use capture::sanitize::Change;
use capture::sanitize::Field;
use capture::sanitize::OpenFlags;
use capture::sanitize::decide::Flag;
use capture::sanitize::decide::Mode;
use capture::share;
use capture::share::BundlePaths;
use capture::share::Clobber;
use capture::share::Decider;
use capture::share::Outcome;
use capture::share::SetupError;
use chrono::Utc;
use config::Config;
use config::Environment;
use config::MappingFile;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionDigest;
use kernel::memory::share::DecideMode;
use kernel::memory::share::DecisionSummary;
use kernel::memory::share::FlagPlace;
use kernel::memory::share::FlagSummary;
use kernel::memory::share::ListedSession;
use kernel::memory::share::OpenedBundle;
use kernel::memory::share::SanitizeJob;
use kernel::memory::share::SanitizeSummary;
use kernel::memory::share::SanitizedBundle;
use kernel::memory::share::Secrets;
use kernel::memory::share::SessionShare;
use kernel::memory::share::ShareFailure;
use kernel::owner::Owner;
use mcp::sessions::Sessions;
use mcp::sessions::ShareFiles;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use crate::service::StartupError;

const SHARE_DIR: &str = "rivethub/share";

#[derive(Debug, Clone)]
pub enum Backend {
    Postgres(Arc<PostgresMemory>),
    Sqlite(SqliteStore),
}

#[derive(Debug)]
struct NodeShare {
    backend: Backend,
    owner: Owner,
    author: BundleAuthor,
    mapping: MappingFile,
    key: PathBuf,
    decider: Result<Decider, SetupError>,
    redaction: Option<Redaction>,
}

pub fn kit(
    config: &Config,
    env: &Environment,
    owner: Owner,
    backend: Backend,
) -> Result<Sessions, StartupError> {
    let node = crate::identity::resolve(config, env)?.name;
    let home = config::Paths::new(env)?.home().to_path_buf();
    let data = match env.text("XDG_DATA_HOME").filter(|text| !text.is_empty()) {
        Some(text) => PathBuf::from(text),
        None => home.join(".local").join("share"),
    };
    let share = NodeShare {
        backend,
        owner: owner.clone(),
        author: BundleAuthor {
            user: owner.as_str().to_owned(),
            node: node.clone(),
        },
        mapping: config.sanitize_mapping(env)?,
        key: share::key_path(env)?,
        decider: share::decider(config, false),
        redaction: capture::redaction::resolve(Some(config.capture_redaction()), env),
    };
    Ok(Sessions {
        share: Arc::new(share),
        files: ShareFiles::new(data.join(SHARE_DIR), home),
        owner,
        node,
    })
}

#[async_trait]
impl SessionShare for NodeShare {
    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, ShareFailure> {
        Ok(match &self.backend {
            Backend::Postgres(store) => store
                .session_digests(&self.owner, keys)
                .await
                .map_err(MemoryStoreError::from)?,
            Backend::Sqlite(store) => store
                .session_digests(&self.owner, keys.to_vec())
                .await
                .map_err(MemoryStoreError::from)?,
        })
    }

    async fn export(&self, ids: &[Uuid], out: PathBuf) -> Result<usize, ShareFailure> {
        let sessions = match &self.backend {
            Backend::Postgres(store) => store
                .bundled_sessions(&self.author, Some(ids))
                .await
                .map_err(MemoryStoreError::from)?,
            Backend::Sqlite(store) => store
                .bundled_sessions(
                    self.author.clone(),
                    Some(ids.iter().map(ToString::to_string).collect()),
                )
                .await
                .map_err(MemoryStoreError::from)?,
        };
        let missing: Vec<String> = ids
            .iter()
            .map(ToString::to_string)
            .filter(|id| {
                !sessions
                    .iter()
                    .any(|session| session.origin.conversation_id.eq_ignore_ascii_case(id))
            })
            .collect();
        if !missing.is_empty() {
            return Err(ShareFailure::UnknownSessions(missing.join(", ")));
        }
        let sessions: Vec<BundledSession> = sessions
            .into_iter()
            .map(BundledSession::forwarding_stored_origin)
            .collect();
        let count = sessions.len();
        let mut text =
            capture::bundle::seal(self.author.clone(), Utc::now(), Sanitization::Raw, sessions)
                .map_err(failed)?
                .text;
        text.push(b'\n');
        share::write_file(out, text, Clobber::Refuse).await?;
        Ok(count)
    }

    async fn sanitize(&self, job: SanitizeJob) -> Result<SanitizeSummary, ShareFailure> {
        let decider = match (job.decide, &self.decider) {
            (false, _) => None,
            (true, Ok(decider)) => Some(decider),
            (true, Err(SetupError::Unconfigured)) => return Err(ShareFailure::DecideUnconfigured),
            (true, Err(error)) => return Err(failed(error)),
        };
        let mapping = job
            .mapping
            .map_or_else(|| self.mapping.clone(), MappingFile::Configured);
        let paths = BundlePaths::beside(&job.out);
        mcp::sessions::refuse_existing(&[&paths.bundle, &paths.report, &paths.local]).await?;
        let bundle = read_bundle(&job.input).await?;
        let outcome = share::sanitize(share::Job {
            bundle: &bundle,
            mapping: &mapping,
            key: &self.key,
            decider,
            paths: &paths,
            clobber: Clobber::Refuse,
            at: Utc::now(),
        })
        .await
        .map_err(failed)?;
        Ok(summary(&outcome, paths))
    }

    async fn open(&self, path: PathBuf) -> Result<OpenedBundle, ShareFailure> {
        let bundle = read_bundle(&path).await?;
        if bundle.manifest.sanitization == Sanitization::Raw {
            return Ok(OpenedBundle::Raw);
        }
        let secrets = capture::sanitize::secrets(&bundle);
        let OpenFlags {
            withheld,
            unreviewed,
        } = capture::sanitize::open_flags(&bundle).map_err(failed)?;
        let mut authors: Vec<String> = Vec::new();
        for (index, session) in bundle.sessions.iter().enumerate() {
            let author = shown(index, &Field::OriginAuthor, author_of(session), &secrets);
            if !authors.contains(&author) {
                authors.push(author);
            }
        }
        Ok(OpenedBundle::Sanitized(SanitizedBundle {
            sessions: bundle
                .sessions
                .iter()
                .enumerate()
                .map(|(index, session)| listed(index, session, &secrets))
                .collect(),
            batches: bundle
                .sessions
                .iter()
                .map(|session| self.batch(session))
                .collect(),
            secrets: secrets.first().map(|first| Secrets {
                count: secrets.len(),
                first: first.to_string(),
            }),
            withheld,
            unreviewed,
            authors,
        }))
    }
}

impl NodeShare {
    fn batch(
        &self,
        session: &BundledSession,
    ) -> Result<kernel::memory::CaptureBatchRequest, ShareFailure> {
        let batch = session.import_batch().map_err(|error| {
            ShareFailure::Failed(format!("cannot build the import batch: {error}"))
        })?;
        let prepared = capture::writer::prepare(&batch, self.redaction.as_ref()).map_err(failed)?;
        Ok(kernel_batch(prepared))
    }
}

fn failed(error: impl std::fmt::Display) -> ShareFailure {
    ShareFailure::Failed(error.to_string())
}

async fn read_bundle(path: &Path) -> Result<SessionBundle, ShareFailure> {
    let not_bundle = || ShareFailure::NotBundle(path.to_path_buf());
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| not_bundle())?;
    capture::bundle::parse(&text).map_err(|_| not_bundle())
}

fn summary(outcome: &Outcome, paths: BundlePaths) -> SanitizeSummary {
    let report = &outcome.report;
    SanitizeSummary {
        sessions: outcome.sanitized.sessions.len(),
        totals: report.totals().to_string(),
        rule_counts: share::rule_counts(report),
        not_scanned: share::NOT_SCANNED.to_owned(),
        best_effort: share::BEST_EFFORT.to_owned(),
        local: outcome.local.then_some(paths.local),
        bundle: paths.bundle,
        report: paths.report,
        decision: report.decision.as_ref().map(|decision| DecisionSummary {
            model: decision.model.clone(),
            scored: decision.scored,
            calls: decision.calls,
            trivial: decision.trivial,
            mode: match decision.review.mode {
                Mode::Advisory => DecideMode::Advisory,
                Mode::Block => DecideMode::Block,
            },
            flags: decision
                .flags
                .iter()
                .map(|flag| flag_summary(flag, outcome))
                .collect(),
        }),
    }
}

fn flag_summary(flag: &Flag, outcome: &Outcome) -> FlagSummary {
    FlagSummary {
        number: flag.number,
        score: flag.score,
        place: flag.places().split_first().map(|(place, more)| FlagPlace {
            session: place.field.session,
            message: place.field.message,
            field: place.field.name.to_string(),
            more: more.len(),
        }),
        report_line: outcome.mark_lines.get(&flag.number).map(|line| line.get()),
        chars: flag.text.chars().count(),
    }
}

fn listed(index: usize, session: &BundledSession, secrets: &[Change]) -> ListedSession {
    let origin = &session.origin;
    ListedSession {
        key: shown(
            index,
            &Field::OriginSessionKey,
            &origin.session_key,
            secrets,
        ),
        agent: shown(index, &Field::OriginAgent, &origin.agent, secrets),
        messages: session.batch.messages.len(),
        author: shown(index, &Field::OriginAuthor, author_of(session), secrets),
        dates: origin.created_at.zip(origin.updated_at),
    }
}

fn shown(index: usize, field: &Field, text: &str, secrets: &[Change]) -> String {
    match secrets
        .iter()
        .find(|change| change.session == Some(index) && change.field == *field)
    {
        Some(change) => change.replacement.clone(),
        None => text.to_owned(),
    }
}

fn author_of(session: &BundledSession) -> &str {
    session.origin.known_author().unwrap_or("an unknown author")
}

fn kernel_batch(batch: wire::capture::CaptureBatchRequest) -> kernel::memory::CaptureBatchRequest {
    kernel::memory::CaptureBatchRequest {
        session_key: batch.session_key,
        agent: batch.agent,
        channel: batch.channel,
        title: batch.title,
        settings: batch.settings,
        task_id: batch.task_id,
        finalize: batch.finalize,
        created_at: batch.created_at,
        updated_at: batch.updated_at,
        messages: batch
            .messages
            .into_iter()
            .map(|message| kernel::memory::CaptureMessage {
                event_id: message.event_id,
                role: message.role,
                content: message.content,
                tool_name: message.tool_name,
                tool_args: message.tool_args,
                tool_result: message.tool_result,
                metadata: message.metadata,
                created_at: message.created_at,
            })
            .collect(),
    }
}

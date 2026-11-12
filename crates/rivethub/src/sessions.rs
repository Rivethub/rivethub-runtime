use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use capture::redaction::Redaction;
use capture::sanitize::OpenFlags;
use capture::sanitize::decide::Flag;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Review;
use capture::share;
use capture::share::BundlePaths;
use capture::share::Clobber;
use capture::share::Decider;
use capture::share::Outcome;
use capture::share::SetupError;
use chrono::Utc;
use cli::memory::SessionStore;
use config::Config;
use config::Environment;
use config::MappingFile;
use kernel::memory::SessionDigest;
use kernel::memory::share::DecideMode;
use kernel::memory::share::DecisionSummary;
use kernel::memory::share::Findings;
use kernel::memory::share::FlagPlace;
use kernel::memory::share::FlagSummary;
use kernel::memory::share::ListedSession;
use kernel::memory::share::OpenedBundle;
use kernel::memory::share::SanitizeJob;
use kernel::memory::share::SanitizeSummary;
use kernel::memory::share::SanitizedBundle;
use kernel::memory::share::SessionShare;
use kernel::memory::share::ShareFailure;
use kernel::owner::Owner;
use mcp::sessions::Sessions;
use mcp::sessions::ShareFiles;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use crate::service::StartupError;

const SHARE_DIR: &str = "rivethub/share";

#[derive(Debug)]
struct NodeShare {
    store: SessionStore,
    owner: Owner,
    author: BundleAuthor,
    mapping: MappingFile,
    key: PathBuf,
    decider: Result<Decider, SetupError>,
    redaction: Option<Redaction>,
    excerpts: Redaction,
}

pub fn kit(
    config: &Config,
    env: &Environment,
    owner: Owner,
    store: SessionStore,
) -> Result<Sessions, StartupError> {
    let node = crate::identity::resolve(config, env)?.name;
    let home = config::Paths::new(env)?.home().to_path_buf();
    let data = match env.text("XDG_DATA_HOME").filter(|text| !text.is_empty()) {
        Some(text) => PathBuf::from(text),
        None => home.join(".local").join("share"),
    };
    let share = NodeShare {
        store,
        owner: owner.clone(),
        author: BundleAuthor {
            user: owner.as_str().to_owned(),
            node: node.clone(),
        },
        mapping: config.sanitize_mapping(env)?,
        key: share::key_path(env)?,
        decider: share::decider(config, false),
        redaction: capture::redaction::resolve(Some(config.capture_redaction()), env),
        excerpts: Redaction::builtins(),
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
        Ok(self.store.digests(&self.owner, keys).await?)
    }

    async fn sanitize(&self, job: SanitizeJob) -> Result<SanitizeSummary, ShareFailure> {
        let decider = match (job.decide, &self.decider) {
            (false, _) => None,
            (true, Ok(decider)) => Some(Decider {
                review: Review {
                    mode: if job.block {
                        Mode::Block
                    } else {
                        decider.review.mode
                    },
                    ..decider.review
                },
                ..decider.clone()
            }),
            (true, Err(SetupError::Unconfigured)) => return Err(ShareFailure::DecideUnconfigured),
            (true, Err(error)) => return Err(failed(error)),
        };
        let mapping = job
            .mapping
            .map_or_else(|| self.mapping.clone(), MappingFile::Configured);
        let paths = BundlePaths::beside(&job.out);
        mcp::sessions::refuse_existing(&[&paths.bundle, &paths.report, &paths.local]).await?;
        let sessions = self
            .store
            .chosen(&self.author, &self.owner, &job.sessions)
            .await?;
        let chosen = share::chosen(self.author.clone(), sessions, Utc::now()).map_err(failed)?;
        let outcome = share::sanitize(share::Job {
            bundle: &chosen.bundle,
            sessions: &chosen.sessions,
            mapping: &mapping,
            key: &self.key,
            decider: decider.as_ref(),
            paths: &paths,
            clobber: Clobber::Refuse,
            at: Utc::now(),
        })
        .await
        .map_err(failed)?;
        Ok(summary(&outcome, paths))
    }

    fn redact(&self, text: &str) -> String {
        self.excerpts.redact_text(text)
    }

    async fn open(&self, path: PathBuf) -> Result<OpenedBundle, ShareFailure> {
        let bundle = read_bundle(&path).await?;
        if bundle.manifest.sanitization == Sanitization::Raw {
            return Ok(OpenedBundle::Raw);
        }
        let checked = capture::receive::check(&bundle);
        let OpenFlags {
            withheld,
            unreviewed,
        } = capture::sanitize::open_flags(&bundle).map_err(failed)?;
        let shown: Vec<&BundledSession> = (0..bundle.sessions.len())
            .filter_map(|index| checked.shown(&bundle, index))
            .collect();
        let mut authors: Vec<String> = Vec::new();
        for session in &shown {
            let author = capture::receive::visible(author_of(session));
            if !authors.contains(&author) {
                authors.push(author);
            }
        }
        Ok(OpenedBundle::Sanitized(SanitizedBundle {
            sessions: shown.iter().map(|session| listed(session)).collect(),
            batches: bundle
                .sessions
                .iter()
                .map(|session| self.batch(session))
                .collect(),
            findings: (!checked.is_clean()).then(|| Findings {
                count: checked.findings().len(),
                sessions: checked.flagged().len(),
                summary: checked.summary(),
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

fn listed(session: &BundledSession) -> ListedSession {
    let origin = &session.origin;
    ListedSession {
        key: capture::receive::visible(&origin.session_key),
        agent: capture::receive::visible(&origin.agent),
        messages: session.batch.messages.len(),
        author: capture::receive::visible(author_of(session)),
        dates: origin.created_at.zip(origin.updated_at),
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

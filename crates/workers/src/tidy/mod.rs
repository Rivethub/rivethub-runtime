mod parse;
mod prompt;

use kernel::topics::SweepOwners;
use std::cmp::Reverse;
use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::filing::Target;
use kernel::model::ModelOptions;
use kernel::model::ModelProvider;
use kernel::model::ModelRequest;
use kernel::tidy::Batch;
use kernel::tidy::Cluster;
use kernel::tidy::Evidence;
use kernel::tidy::Fingerprint;
use kernel::tidy::Guard;
use kernel::tidy::MIN_SESSIONS;
use kernel::tidy::Proposer;
use kernel::tidy::Reason;
use kernel::tidy::Sample;
use kernel::tidy::TidyStore;
use kernel::tidy::TreeEdit;
use kernel::tidy::WEEK;
use kernel::tidy::clusters;
use kernel::tidy::duplicates;
use kernel::topics::Slug;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use tokio_util::sync::CancellationToken;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;

pub use prompt::TIDY_SYSTEM_PROMPT;

use crate::SessionTopics;
use crate::error::WorkerError;
use crate::summarize::llm_error;
use parse::Line;

const MAX_TIDY_TOKENS: u32 = 2_000;
const CORRECTION_EXAMPLES: u32 = 8;
const SAMPLES: usize = 3;

#[expect(
    missing_debug_implementations,
    reason = "the topic and tidy ports and the model provider are trait objects"
)]
pub struct TidyEnv<'a> {
    pub topics: &'a SessionTopics,
    pub tidy: &'a dyn TidyStore,
    pub provider: &'a dyn ModelProvider,
    pub model: &'a str,
    pub owners: SweepOwners<'a>,
    pub now: DateTime<Utc>,
    pub cancel: &'a CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TidyRun {
    NotDue { last: DateTime<Utc> },
    Proposed(Batch),
}

struct Candidate<'a> {
    change: TreeEdit,
    reason: Reason,
    cluster: Option<&'a Cluster>,
}

#[derive(Debug, thiserror::Error)]
enum Unresolved {
    #[error("{0} is not listed")]
    Reference(String),
    #[error(transparent)]
    Name(#[from] kernel::topics::TopicError),
    #[error(transparent)]
    Reason(#[from] kernel::tidy::TidyError),
}

pub async fn handle_tidy(env: &TidyEnv<'_>) -> Result<TidyRun, WorkerError> {
    if let Some(last) = env.tidy.last_batch(Proposer::Categorizer).await?
        && env.now - last < WEEK
    {
        return Ok(TidyRun::NotDue { last });
    }
    let mut listings = env.topics.store.list().await?;
    listings.retain(|listing| listing.topic.owner.is_none() || env.owners.includes(&listing.topic));
    let known = env.tidy.known().await?;
    let inputs = env.tidy.inputs(env.now - WEEK).await?;
    let corrections = env.topics.filing.corrections(CORRECTION_EXAMPLES).await?;
    let (suggested, doubled) = shown(env, &listings, &known, inputs.suggestions).await?;
    let mut refs = prompt::Refs::of(&listings, &inputs.recent);
    let user = prompt::user_prompt(
        &mut refs,
        &listings,
        &prompt::Shown {
            clusters: &suggested,
            duplicates: &doubled,
            corrections: &corrections,
        },
    );
    let lines = match chat(env, &user).await? {
        Some(reply) => parse::lines(&reply)?,
        None => Vec::new(),
    };
    let mut candidates = Vec::new();
    for line in lines {
        let candidate = match resolve(line, &refs, &suggested) {
            Ok(candidate) => Some(candidate),
            Err(unresolved) => {
                tracing::info!("tidy: dropped an answer line: {unresolved}");
                None
            }
        };
        candidates.extend(candidate);
    }
    let batch = admit(env, &listings, &known, candidates).await?;
    let stored = env.tidy.propose(batch).await?;
    tracing::info!("tidy: proposed {} changes", stored.proposals.len());
    Ok(TidyRun::Proposed(stored))
}

async fn shown(
    env: &TidyEnv<'_>,
    listings: &[TopicListing],
    known: &BTreeSet<Fingerprint>,
    suggestions: Vec<kernel::tidy::Suggested>,
) -> Result<(Vec<Cluster>, Vec<(TopicId, TopicId)>), WorkerError> {
    let suggested: Vec<Cluster> = clusters(suggestions)
        .into_iter()
        .filter(|cluster| cluster.sessions.len() >= MIN_SESSIONS)
        .collect();
    let drafts: Vec<TreeEdit> = suggested
        .iter()
        .map(|cluster| create(cluster, cluster.name.clone(), cluster.parent))
        .collect();
    let tombstoned = tombstones(env, listings, known, &drafts).await?;
    let guard = Guard::new(listings, known, &tombstoned);
    let suggested = suggested
        .into_iter()
        .zip(drafts)
        .filter(|(_, draft)| guard.check(draft).is_ok())
        .map(|(cluster, _)| cluster)
        .collect();
    let doubled = duplicates(listings)
        .into_iter()
        .filter(|(source, into)| {
            guard
                .check(&TreeEdit::Merge {
                    source: *source,
                    into: *into,
                })
                .is_ok()
        })
        .collect();
    Ok((suggested, doubled))
}

async fn admit(
    env: &TidyEnv<'_>,
    listings: &[TopicListing],
    known: &BTreeSet<Fingerprint>,
    candidates: Vec<Candidate<'_>>,
) -> Result<Batch, WorkerError> {
    let changes: Vec<TreeEdit> = candidates
        .iter()
        .map(|candidate| candidate.change.clone())
        .collect();
    let tombstoned = tombstones(env, listings, known, &changes).await?;
    let guard = Guard::new(listings, known, &tombstoned);
    let mut batch = Batch::new(Proposer::Categorizer, env.now);
    let mut seen: BTreeSet<Fingerprint> = BTreeSet::new();
    for candidate in candidates {
        if let Err(refused) = guard.check(&candidate.change) {
            tracing::info!("tidy: refused {:?}: {refused:?}", candidate.change);
            continue;
        }
        if !seen.insert(candidate.change.fingerprint()?) {
            continue;
        }
        let evidence = evidence(env, &candidate.change, candidate.cluster).await?;
        batch.add(candidate.change, candidate.reason, evidence);
    }
    Ok(batch)
}

fn create(cluster: &Cluster, name: TopicName, parent: Option<TopicId>) -> TreeEdit {
    TreeEdit::CreateTopic {
        topic: TopicId::random(),
        name,
        parent,
        aliases: Vec::new(),
        members: cluster
            .sessions
            .iter()
            .map(|suggested| Target::Session(suggested.session.clone()))
            .collect(),
    }
}

async fn tombstones(
    env: &TidyEnv<'_>,
    listings: &[TopicListing],
    known: &BTreeSet<Fingerprint>,
    changes: &[TreeEdit],
) -> Result<BTreeSet<(TopicKind, Slug)>, WorkerError> {
    let none = BTreeSet::new();
    let guard = Guard::new(listings, known, &none);
    let mut tombstoned = BTreeSet::new();
    for (kind, slug) in changes.iter().filter_map(|change| guard.named(change)) {
        if env.topics.store.tombstoned(kind, &slug).await? {
            tombstoned.insert((kind, slug));
        }
    }
    Ok(tombstoned)
}

fn resolve<'a>(
    line: Line,
    refs: &prompt::Refs,
    clusters: &'a [Cluster],
) -> Result<Candidate<'a>, Unresolved> {
    let topic = |reference: &str| {
        refs.topic(reference)
            .ok_or_else(|| Unresolved::Reference(reference.to_owned()))
    };
    let under = |reference: Option<String>| reference.as_deref().map(topic).transpose();
    let mut cluster = None;
    let (change, reason) = match line {
        Line::Merge {
            merge,
            into,
            reason,
        } => (
            TreeEdit::Merge {
                source: topic(&merge)?,
                into: topic(&into)?,
            },
            reason,
        ),
        Line::Move {
            topic: moved,
            under: parent,
            reason,
        } => (
            TreeEdit::Move {
                topic: topic(&moved)?,
                parent: under(parent)?,
            },
            reason,
        ),
        Line::Rename { rename, to, reason } => (
            TreeEdit::Rename {
                topic: topic(&rename)?,
                name: TopicName::new(&to)?,
            },
            reason,
        ),
        Line::Close { close, reason } => (
            TreeEdit::Close {
                topic: topic(&close)?,
            },
            reason,
        ),
        Line::Delete { delete, reason } => (
            TreeEdit::Delete {
                topic: topic(&delete)?,
            },
            reason,
        ),
        Line::Create {
            create: reference,
            name,
            under: parent,
            reason,
        } => {
            let found = cluster_of(&reference, clusters)?;
            cluster = Some(found);
            (named_cluster(found, name, parent, refs)?, reason)
        }
        Line::Type {
            name,
            under: parent,
            reason,
        } => (
            TreeEdit::AddType {
                topic: TopicId::random(),
                name: TopicName::new(&name)?,
                parent: under(parent)?,
            },
            reason,
        ),
    };
    Ok(Candidate {
        change,
        reason: Reason::new(&reason)?,
        cluster,
    })
}

fn named_cluster(
    cluster: &Cluster,
    name: Option<String>,
    parent: Option<String>,
    refs: &prompt::Refs,
) -> Result<TreeEdit, Unresolved> {
    let name = match name {
        Some(name) => TopicName::new(&name)?,
        None => cluster.name.clone(),
    };
    let parent = match parent {
        Some(parent) => Some(
            refs.topic(&parent)
                .ok_or_else(|| Unresolved::Reference(parent.clone()))?,
        ),
        None => cluster.parent,
    };
    Ok(create(cluster, name, parent))
}

fn cluster_of<'a>(reference: &str, clusters: &'a [Cluster]) -> Result<&'a Cluster, Unresolved> {
    let unknown = || Unresolved::Reference(reference.to_owned());
    let index: usize = reference
        .trim()
        .strip_prefix('C')
        .ok_or_else(unknown)?
        .parse()
        .map_err(|_| unknown())?;
    index
        .checked_sub(1)
        .and_then(|index| clusters.get(index))
        .ok_or_else(unknown)
}

async fn evidence(
    env: &TidyEnv<'_>,
    change: &TreeEdit,
    cluster: Option<&Cluster>,
) -> Result<Evidence, WorkerError> {
    let topic = match change {
        TreeEdit::Merge { source: topic, .. }
        | TreeEdit::Move { topic, .. }
        | TreeEdit::Rename { topic, .. }
        | TreeEdit::Close { topic }
        | TreeEdit::Delete { topic } => *topic,
        TreeEdit::AddType { .. } | TreeEdit::MergePage { .. } | TreeEdit::RemovePage { .. } => {
            return Ok(Evidence::default());
        }
        TreeEdit::FileInto { members, .. } => {
            return Ok(Evidence {
                sessions: u64::try_from(members.len()).map_err(|_| WorkerError::Count)?,
                samples: Vec::new(),
            });
        }
        TreeEdit::CreateTopic { members, .. } => {
            return Ok(Evidence {
                sessions: u64::try_from(members.len()).map_err(|_| WorkerError::Count)?,
                samples: cluster
                    .into_iter()
                    .flat_map(|cluster| cluster.sessions.iter())
                    .take(SAMPLES)
                    .map(|suggested| Sample {
                        id: suggested.session.as_str().to_owned(),
                        title: suggested.title.clone(),
                        last_active: None,
                    })
                    .collect(),
            });
        }
    };
    let mut filed = env.topics.filing.filed(topic).await?;
    let total = filed.sessions.len() + filed.conversations.len();
    filed.sessions.sort_by_key(|row| Reverse(row.last_active));
    Ok(Evidence {
        sessions: u64::try_from(total).map_err(|_| WorkerError::Count)?,
        samples: filed
            .sessions
            .into_iter()
            .take(SAMPLES)
            .map(|row| Sample {
                id: row.placement.session.as_str().to_owned(),
                title: row.title,
                last_active: row.last_active,
            })
            .collect(),
    })
}

async fn chat(env: &TidyEnv<'_>, user: &str) -> Result<Option<String>, WorkerError> {
    if env.cancel.is_cancelled() {
        return Err(WorkerError::Cancelled);
    }
    let request = ModelRequest {
        options: ModelOptions::default(),
        model: env.model.to_owned(),
        messages: vec![
            text_message(Role::System, TIDY_SYSTEM_PROMPT),
            text_message(Role::User, user),
        ],
        tools: Vec::new(),
        thinking: None,
        max_tokens: Some(MAX_TIDY_TOKENS),
    };
    let response = tokio::select! {
        biased;
        () = env.cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = env.provider.chat(request) => result.map_err(llm_error)?,
    };
    Ok(response.content)
}

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: MessageContent::Text(text.to_owned()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

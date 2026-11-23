use kernel::wiki::entity_pass::AppliedMark;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityPage;
use kernel::wiki::entity_pass::EntityRewrite;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::LeafClock;
use kernel::wiki::entity_pass::LiveFact;
use kernel::wiki::entity_pass::MentionNote;
use kernel::wiki::entity_pass::PublishOutcome;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::entity_pass::TranscriptMessage;
use kernel::wiki::entity_pass::older_than_mark;
use kernel::wiki::entity_pass::parse_rewrite;
use kernel::wiki::entity_pass::prose_article;
use kernel::wiki::entity_pass::reconcile_facts;
use kernel::wiki::entity_pass::rewrite_schema;
use kernel::wiki::entity_pass::session_history;
use kernel::wiki::entity_vocab::VocabEntry;
use kernel::wiki::entity_vocab::vocabulary;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::sync::SemaphorePermit;
use wire::time::TimestampFormat as _;

use super::super::EntityBuilderEnv;
use super::super::gather;
use super::REWRITE_MAX_TOKENS;
use crate::WorkerError;

use super::report::owner_of;
use super::report::record_empty;
use super::report::record_entity_error;
use super::report::record_no_evidence;
use super::report::record_reconciliation;

const OLDER_EVIDENCE_PROMPT: &str = "\
This call is older evidence. Instructions above that say the latest passage wins apply inside this session only. The current page already reflects later sessions. Only facts with no newer counterpart may be added. Do not replace a value a later session established. history_sentence describes this older session and is filed in date order.";

const REWRITE_CONCURRENCY: usize = 2;

const REWRITE_CONCURRENCY_MAX: usize = 64;

#[derive(Debug, Clone)]
pub struct RewriteLimit {
    permits: Arc<Semaphore>,
}

impl RewriteLimit {
    pub fn from_config(config: &config::Config) -> Result<Self, String> {
        let Some(value) = config
            .entities
            .as_ref()
            .and_then(|section| section.rewrite_concurrency)
        else {
            return Ok(Self::default());
        };
        let out_of_range = || {
            format!(
                "entities.rewrite_concurrency must be between 1 and {REWRITE_CONCURRENCY_MAX}, not {value}"
            )
        };
        let permits = usize::try_from(value).map_err(|_| out_of_range())?;
        if !(1..=REWRITE_CONCURRENCY_MAX).contains(&permits) {
            return Err(out_of_range());
        }
        Ok(Self::with_permits(permits))
    }

    fn with_permits(permits: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits)),
        }
    }
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl Default for RewriteLimit {
    fn default() -> Self {
        Self::with_permits(REWRITE_CONCURRENCY)
    }
}

#[derive(Debug)]
pub(in crate::entity_builder) struct StoredPage {
    pub(in crate::entity_builder) git_sha: String,
    pub(in crate::entity_builder) slug: String,
}

pub(in crate::entity_builder) struct TouchedEntity {
    pub(in crate::entity_builder) view: EntityView,
    pub(in crate::entity_builder) mention: String,
}

pub(in crate::entity_builder) enum AcceptedRewrite {
    Page(StoredRewrite),

    Empty(String),
}

#[derive(Serialize)]
struct ShownFact<'a> {
    attribute: &'a str,
    value: &'a str,
    as_of: String,
    status: &'a str,
    fixed: bool,
}

#[derive(Serialize)]
struct RewriteInput<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    type_name: &'a str,
    aliases: &'a [String],
    session_start: &'a str,
    session_end: &'a str,
    summary: &'a str,
    article: &'a str,
    current_facts: Vec<ShownFact<'a>>,
    passages: &'a str,
    vocabulary: &'a [VocabEntry],
    older_evidence: bool,
    observed_types: &'a [String],
}

pub(in crate::entity_builder) async fn rewrite_touched(
    env: &EntityBuilderEnv<'_>,
    leaves: &[EntityLeaf],
    messages: &[TranscriptMessage],
    touched: &[TouchedEntity],
) -> Result<Vec<StoredPage>, WorkerError> {
    let results = futures::future::join_all(
        touched
            .iter()
            .map(|item| rewrite_one(env, leaves, messages, &item.view, &item.mention)),
    )
    .await;
    let mut stored = Vec::new();
    let mut fatal = None;
    for (item, result) in touched.iter().zip(results) {
        match result {
            Ok(Some(page)) => stored.push(page),
            Ok(None) => {}
            Err(error) => note_failure(item, error, &mut fatal),
        }
    }
    match fatal {
        Some(error) => Err(error),
        None => Ok(stored),
    }
}

fn note_failure(item: &TouchedEntity, error: WorkerError, fatal: &mut Option<WorkerError>) {
    if job_fatal(&error) {
        if fatal.is_none() {
            *fatal = Some(error);
        }
        return;
    }
    let detail = error.to_string();
    tracing::warn!(
        entity = %item.view.slug,
        error = %detail,
        "extract-entities: entity failed; the rest of the session continues"
    );
    record_entity_error(&item.view.name, &detail);
}

fn job_fatal(error: &WorkerError) -> bool {
    #[expect(clippy::wildcard_enum_match_arm)]
    match error {
        WorkerError::Cancelled
        | WorkerError::Closed
        | WorkerError::ModelTimeout { .. }
        | WorkerError::ProviderOutage { .. }
        | WorkerError::WikiModel(_)
        | WorkerError::WikiJson(_) => true,
        WorkerError::EntityStore(source) => store_retries(source),
        _ => false,
    }
}

fn store_retries(error: &EntityStoreError) -> bool {
    matches!(
        error,
        EntityStoreError::Query(_) | EntityStoreError::Page(_)
    )
}

async fn rewrite_one(
    env: &EntityBuilderEnv<'_>,
    leaves: &[EntityLeaf],
    messages: &[TranscriptMessage],
    view: &EntityView,
    mention: &str,
) -> Result<Option<StoredPage>, WorkerError> {
    let passages = gather::passages(messages, &view.name, &view.aliases, mention);
    if passages.trim().is_empty() {
        record_no_evidence(&view.name);
        return Ok(None);
    }
    let permit = acquire(env).await?;
    let mut lease = env.store.lease_entity(view.id).await?;
    let outcome = rewrite_open(env, leaves, view, &passages, lease.as_mut(), mention).await;
    let released = lease.release().await.map_err(WorkerError::from);
    drop(permit);
    match (outcome, released) {
        (Ok(page), Ok(())) => Ok(page),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

async fn acquire<'a>(env: &EntityBuilderEnv<'a>) -> Result<SemaphorePermit<'a>, WorkerError> {
    let waiting = env.rewrite.permits.acquire();
    tokio::select! {
        biased;
        () = env.cancel.cancelled() => Err(WorkerError::Cancelled),
        result = waiting => result.map_err(|_| WorkerError::Closed),
    }
}

async fn rewrite_open(
    env: &EntityBuilderEnv<'_>,
    leaves: &[EntityLeaf],
    view: &EntityView,
    passages: &str,
    lease: &mut dyn kernel::wiki::entity_pass::EntityLease,
    mention: &str,
) -> Result<Option<StoredPage>, WorkerError> {
    if let Some(session) = session_of(leaves) {
        let mark = lease.applied_through().await?;
        if already_applied(session, mark) {
            return Ok(Some(StoredPage {
                git_sha: String::new(),
                slug: view.slug.clone(),
            }));
        }
    }
    rewrite_held(env, leaves, view, passages, lease, mention).await
}

fn already_applied(session: AppliedMark, mark: Option<AppliedMark>) -> bool {
    mark.is_some_and(|mark| (session.at, session.summary_id) == (mark.at, mark.summary_id))
}

async fn rewrite_held(
    env: &EntityBuilderEnv<'_>,
    leaves: &[EntityLeaf],
    view: &EntityView,
    passages: &str,
    lease: &mut dyn kernel::wiki::entity_pass::EntityLease,
    mention: &str,
) -> Result<Option<StoredPage>, WorkerError> {
    let page = lease.page().await?;
    let mark = lease.applied_through().await?;
    let older = session_of(leaves).is_some_and(|session| mark_is_ahead(session, mark));
    match accepted_rewrite(env, view, leaves, &page, passages, older, mention).await? {
        AcceptedRewrite::Page(body) => store_page(env, view, lease, &body).await.map(Some),
        AcceptedRewrite::Empty(sentence) => {
            note_empty(env, view, leaves, lease, &page, &sentence).await
        }
    }
}

async fn store_page(
    env: &EntityBuilderEnv<'_>,
    view: &EntityView,
    lease: &mut dyn kernel::wiki::entity_pass::EntityLease,
    body: &StoredRewrite,
) -> Result<StoredPage, WorkerError> {
    let stored = match lease.apply(body, env.now).await? {
        PublishOutcome::Stored { git_sha, slug } => StoredPage { git_sha, slug },
        PublishOutcome::Stale(_) => {
            return Err(WorkerError::Llm {
                message: format!("entity {} changed while its page was rewritten", view.slug),
            });
        }
    };
    super::super::resolve::refresh_embedding(env, view, &body.summary).await;
    Ok(stored)
}

async fn note_empty(
    env: &EntityBuilderEnv<'_>,
    view: &EntityView,
    leaves: &[EntityLeaf],
    lease: &mut dyn kernel::wiki::entity_pass::EntityLease,
    page: &EntityPage,
    sentence: &str,
) -> Result<Option<StoredPage>, WorkerError> {
    let fresh = page_is_blank(page);
    record_empty(&view.name, fresh);
    if fresh {
        return Ok(None);
    }
    let note = mention_note(view, leaves, sentence)?;
    let sha = lease.record_mention(&note, env.now).await?;
    Ok(sha.map(|git_sha| StoredPage {
        git_sha,
        slug: view.slug.clone(),
    }))
}

fn page_is_blank(page: &EntityPage) -> bool {
    page.summary.trim().is_empty() && page.article.trim().is_empty() && page.facts.is_empty()
}

fn mention_note(
    view: &EntityView,
    leaves: &[EntityLeaf],
    sentence: &str,
) -> Result<MentionNote, WorkerError> {
    let Some(lead) = leaves.first() else {
        return Err(WorkerError::Llm {
            message: format!("entity {} has no session", view.slug),
        });
    };
    Ok(MentionNote {
        history: session_history(
            lead.id,
            lead.conversation_id,
            lead.start.utc_date(),
            sentence,
        ),
        session_at: lead.start,
        session_id: lead.id,
        owner: owner_of(view, leaves)?,
    })
}

#[expect(clippy::too_many_arguments)]
pub(in crate::entity_builder) async fn accepted_rewrite(
    env: &super::super::EntityBuilderEnv<'_>,
    view: &EntityView,
    leaves: &[EntityLeaf],
    page: &EntityPage,
    passages: &str,
    older: bool,
    mention: &str,
) -> Result<AcceptedRewrite, WorkerError> {
    let user = rewrite_user(view, leaves, page, passages, older)?;
    match rewrite_attempt(env, view, leaves, page, &user, passages, older, mention).await {
        Ok(body) => return Ok(AcceptedRewrite::Page(body)),
        Err(AttemptError::Empty(_)) => {
            let again = super::super::retry::retry_after(
                env.cancel,
                Err::<(), _>(WorkerError::EmptySummary),
            )
            .await?;
            if again.is_some() {
                return Err(WorkerError::Llm {
                    message: "an empty rewrite retry produced a page".to_owned(),
                });
            }
        }
        Err(AttemptError::Failed(error)) => {
            if let Some(body) =
                super::super::retry::retry_after(env.cancel, Err::<StoredRewrite, _>(error)).await?
            {
                return Ok(AcceptedRewrite::Page(body));
            }
        }
    }
    match rewrite_attempt(env, view, leaves, page, &user, passages, older, mention).await {
        Ok(body) => return Ok(AcceptedRewrite::Page(body)),
        Err(AttemptError::Empty(sentence)) => return Ok(AcceptedRewrite::Empty(sentence)),
        Err(AttemptError::Failed(error)) => {
            if let Some(body) =
                super::super::retry::last_chance(env.cancel, Err::<StoredRewrite, _>(error)).await?
            {
                return Ok(AcceptedRewrite::Page(body));
            }
        }
    }
    match rewrite_attempt(env, view, leaves, page, &user, passages, older, mention).await {
        Ok(body) => Ok(AcceptedRewrite::Page(body)),
        Err(AttemptError::Empty(sentence)) => Ok(AcceptedRewrite::Empty(sentence)),
        Err(AttemptError::Failed(error)) => Err(error),
    }
}

fn rewrite_user(
    view: &EntityView,
    leaves: &[EntityLeaf],
    page: &EntityPage,
    passages: &str,
    older: bool,
) -> Result<String, WorkerError> {
    let (session_start, session_end) = session_range(leaves);
    serde_json::to_string(&RewriteInput {
        name: &view.name,
        type_name: &view.type_name,
        aliases: &view.aliases,
        session_start: &session_start,
        session_end: &session_end,
        summary: &page.summary,
        article: &page.article,
        current_facts: shown(&page.facts, &page.fixed),
        passages,
        vocabulary: vocabulary(),
        older_evidence: older,
        observed_types: &page.observed_types,
    })
    .map_err(WorkerError::WikiJson)
}

enum AttemptError {
    Empty(String),
    Failed(WorkerError),
}

pub(super) fn rewrite_prompt(older: bool) -> String {
    if older {
        format!(
            "{}\n\n{OLDER_EVIDENCE_PROMPT}",
            super::super::ENTITY_REWRITE_SYSTEM_PROMPT
        )
    } else {
        super::super::ENTITY_REWRITE_SYSTEM_PROMPT.to_owned()
    }
}

fn session_of(leaves: &[EntityLeaf]) -> Option<AppliedMark> {
    let lead = leaves.first()?;
    Some(AppliedMark {
        at: lead.start,
        summary_id: lead.id,
    })
}

fn mark_is_ahead(session: AppliedMark, mark: Option<AppliedMark>) -> bool {
    mark.is_some_and(|mark| older_than_mark(session.at, session.summary_id, mark))
}

#[expect(clippy::too_many_arguments)]
async fn rewrite_attempt(
    env: &super::super::EntityBuilderEnv<'_>,
    view: &EntityView,
    leaves: &[EntityLeaf],
    page: &EntityPage,
    user: &str,
    passages: &str,
    older: bool,
    mention: &str,
) -> Result<StoredRewrite, AttemptError> {
    let text = super::super::chat(
        env,
        &rewrite_prompt(older),
        rewrite_schema(),
        user,
        REWRITE_MAX_TOKENS,
    )
    .await
    .map_err(AttemptError::Failed)?;
    let rewrite =
        parse_rewrite(&text).map_err(|error| AttemptError::Failed(WorkerError::WikiJson(error)))?;
    if rewrite.summary.trim().is_empty() && rewrite.article.trim().is_empty() {
        return Err(AttemptError::Empty(rewrite.history_sentence));
    }
    stored_rewrite(page, &rewrite, view, leaves, passages, mention).map_err(AttemptError::Failed)
}

fn shown<'a>(facts: &'a [LiveFact], fixed: &[String]) -> Vec<ShownFact<'a>> {
    facts
        .iter()
        .map(|fact| ShownFact {
            attribute: &fact.attribute,
            value: &fact.value,
            as_of: fact.as_of.rfc3339_offset(),
            status: fact.status.as_str(),
            fixed: fixed.iter().any(|key| key == &fact.attribute),
        })
        .collect()
}

fn session_range(leaves: &[EntityLeaf]) -> (String, String) {
    let start = leaves.iter().map(|leaf| leaf.start).min();
    let end = leaves.iter().map(|leaf| leaf.end).max();
    (
        start
            .map(wire::time::TimestampFormat::rfc3339_offset)
            .unwrap_or_default(),
        end.map(wire::time::TimestampFormat::rfc3339_offset)
            .unwrap_or_default(),
    )
}

pub(in crate::entity_builder) fn stored_rewrite(
    page: &EntityPage,
    rewrite: &EntityRewrite,
    view: &EntityView,
    leaves: &[EntityLeaf],
    passages: &str,
    mention: &str,
) -> Result<StoredRewrite, WorkerError> {
    let summary = rewrite.summary.trim();
    if summary.is_empty() {
        return Err(WorkerError::EmptySummary);
    }
    let Some(lead) = leaves.first() else {
        return Err(WorkerError::Llm {
            message: format!("entity {} has no session", view.slug),
        });
    };
    let clocks: Vec<LeafClock> = leaves.iter().map(LeafClock::from).collect();
    let reconciled = reconcile_facts(
        &view.type_name,
        &page.facts,
        rewrite,
        lead.id,
        lead.conversation_id,
        &clocks,
        passages,
    );
    if reconciled.facts.is_empty() && !page.facts.is_empty() {
        return Err(WorkerError::Llm {
            message: format!("rewrite of {} dropped every current fact", view.slug),
        });
    }
    record_reconciliation(&view.slug, &reconciled);
    Ok(StoredRewrite {
        summary: summary.to_owned(),
        article: prose_article(&rewrite.article),
        facts: reconciled.facts,
        history: session_history(
            lead.id,
            lead.conversation_id,
            lead.start.utc_date(),
            &reconciled.history_sentence,
        ),
        owner: owner_of(view, leaves)?,
        session_at: lead.start,
        session_id: lead.id,
        evidence: mention.to_owned(),
    })
}

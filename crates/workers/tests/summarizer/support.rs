use super::*;

pub(super) fn bind<'a>(
    store: &'a MemStore,
    script: &'a Scripted,
    jobs: &'a Seen,
    settings: &'a SummarySettings,
    now: DateTime<Utc>,
    token: &'a CancellationToken,
) -> SummarizeEnv<'a> {
    SummarizeEnv {
        store,
        provider: script,
        queue: jobs,
        settings,
        now,
        cancel: token,
    }
}

pub(super) fn assert_calls(calls: &[Captured], expected: &[ChatCall]) {
    let calls: Vec<&Captured> = calls
        .iter()
        .filter(|call| call.kind != "TITLE_SYSTEM_PROMPT")
        .collect();
    assert_eq!(calls.len(), expected.len());
    for (call, recorded) in calls.iter().zip(expected) {
        assert_eq!(call.kind, recorded.prompt);
        assert_eq!(call.user, recorded.user);
        assert_eq!(call.model, recorded.model.as_deref().unwrap_or("fake-chat"));
        if let Some(tokens) = recorded.max_tokens {
            assert_eq!(call.max_tokens, Some(tokens));
        }
    }
}

pub(super) fn assert_rows(written: &[NewSummary], rows: &[SummaryRow]) {
    assert_eq!(written.len(), rows.len());
    for summary in written {
        let row = rows
            .iter()
            .find(|row| {
                row.kind == summary.kind.as_str()
                    && row.earliest_at == summary.earliest_at
                    && row.latest_at == summary.latest_at
                    && row.message_count == summary.message_count
            })
            .expect("matching summary");
        assert_eq!(summary.depth, row.depth);
        assert_eq!(summary.content, row.content);
        assert_eq!(summary.model, row.model);
        assert_eq!(summary.pipeline_version, row.pipeline_version);
        assert_eq!(summary.created_at, row.created_at);
        assert_eq!(summary.conversation_id, row.conversation_id);
        let linked = written
            .iter()
            .any(|parent| parent.child_ids.contains(&summary.id));
        assert_eq!(linked, row.parent_id.is_some());
    }
}

pub(super) fn assert_sources(written: &[NewSummary], messages: &[WindowMessage]) {
    let mut cursor = 0usize;
    for summary in written
        .iter()
        .filter(|summary| summary.kind == SummaryKind::Leaf)
    {
        for (ordinal, source) in summary.sources.iter().enumerate() {
            assert_eq!(i32::try_from(ordinal).expect("ordinal"), source.ordinal);
            let message = messages.get(cursor).expect("source message");
            assert_eq!(source.message_id, message.id);
            cursor = cursor.saturating_add(1);
        }
    }
    assert_eq!(cursor, messages.len());
}

pub(super) async fn assert_health(
    store: &MemStore,
    settings: &SummarySettings,
    expected: &HealthSnap,
) {
    let health =
        summary_health(&store.pending().await.expect("pending"), settings).expect("health");
    assert_eq!(
        health,
        SummaryHealth {
            eligible: expected.eligible,
            active_tail: expected.active_tail,
            below_floor: expected.below_floor,
        }
    );
}

pub(super) fn summarized_counts(logs: &[String]) -> Vec<u32> {
    logs.iter()
        .filter_map(|line| summarized_count(line))
        .collect()
}

pub(super) fn summarized_count(line: &str) -> Option<u32> {
    let tail = line.rsplit_once(": ").map(|(_, tail)| tail)?;
    let number = tail.strip_suffix(" summaries")?;
    number.parse().ok()
}

impl MemStore {
    pub(super) fn new(windows: Vec<ConversationWindow>) -> Self {
        Self {
            windows: Mutex::new(windows),
            written: Mutex::new(Vec::new()),
            retitled: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl SummaryStore for MemStore {
    async fn conversation(
        &self,
        id: Uuid,
    ) -> Result<Option<ConversationWindow>, SummaryStoreError> {
        Ok(self
            .windows
            .lock()
            .await
            .iter()
            .find(|window| window.conversation.id == id)
            .cloned())
    }

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError> {
        let mut pending = self
            .windows
            .lock()
            .await
            .iter()
            .filter(|window| {
                !window.messages.is_empty()
                    && !window
                        .conversation
                        .session_key
                        .starts_with(kernel::memory::HEARTBEAT_SESSION_PREFIX)
            })
            .map(|window| PendingConversation {
                id: window.conversation.id,
                unsummarized: u32::try_from(window.messages.len()).expect("message count"),
                latest_message_at: window.latest_message_at,
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|conversation| std::cmp::Reverse(conversation.latest_message_at));
        Ok(pending)
    }

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError> {
        let node = SummaryNode {
            id: summary.id,
            kind: summary.kind,
            content: summary.content.clone(),
            message_count: summary.message_count,
            earliest_at: summary.earliest_at,
            latest_at: summary.latest_at,
            depth: summary.depth,
        };
        let mut windows = self.windows.lock().await;
        let window = windows
            .iter_mut()
            .find(|window| window.conversation.id == summary.conversation_id)
            .ok_or_else(|| SummaryStoreError::Missing {
                id: summary.conversation_id.to_string(),
            })?;
        match summary.kind {
            SummaryKind::Leaf => {
                for source in &summary.sources {
                    window
                        .messages
                        .retain(|message| message.id != source.message_id);
                }
                window.leaves.push(node);
            }
            SummaryKind::Branch => {
                window
                    .leaves
                    .retain(|leaf| !summary.child_ids.contains(&leaf.id));
                window.branches.push(node);
            }
            SummaryKind::Root => {
                window
                    .branches
                    .retain(|branch| !summary.child_ids.contains(&branch.id));
            }
        }
        drop(windows);
        let mut written = self.written.lock().await;
        let first_leaf = summary.kind == SummaryKind::Leaf
            && !written.iter().any(|earlier| {
                earlier.kind == SummaryKind::Leaf
                    && earlier.conversation_id == summary.conversation_id
            });
        written.push(summary);
        Ok(if first_leaf {
            SummaryWrite::FirstLeaf
        } else {
            SummaryWrite::Added
        })
    }

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError> {
        let needs = self.windows.lock().await.iter().any(|window| {
            window.conversation.id == id
                && kernel::memory::needs_title(window.conversation.title.as_deref())
        });
        if needs {
            self.retitled.lock().await.push((id, title.to_owned()));
        }
        Ok(needs)
    }
}

impl Scripted {
    pub(super) fn success() -> Self {
        Self {
            failures: Mutex::new(Vec::new()),
            truncations: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn truncate_once() -> Self {
        Self {
            failures: Mutex::new(Vec::new()),
            truncations: Mutex::new(1),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn fail_with(statuses: Vec<u16>) -> Self {
        Self {
            failures: Mutex::new(statuses),
            truncations: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ModelProvider for Scripted {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let kind = kind_name(system_text(&request)?);
        let user = user_text(&request)?;
        self.calls.lock().await.push(Captured {
            kind: kind.to_owned(),
            user: user.to_owned(),
            max_tokens: request.max_tokens,
            model: request.model.clone(),
        });
        let status = {
            let mut failures = self.failures.lock().await;
            if failures.is_empty() {
                None
            } else {
                Some(failures.remove(0))
            }
        };
        if let Some(status) = status {
            return Err(status_error(status));
        }
        let mut truncations = self.truncations.lock().await;
        if *truncations > 0 {
            *truncations -= 1;
            return Ok(response(String::new(), Some(FinishReason::Length)));
        }
        drop(truncations);
        Ok(response(answer(kind), Some(FinishReason::Stop)))
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(plain_error("the summarizer script has no stream"))
    }

    async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        Err(plain_error("the summarizer script has no embeddings"))
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

pub(super) fn system_text(request: &ModelRequest) -> Result<&str, ModelProviderError> {
    let message = request
        .messages
        .first()
        .ok_or_else(|| plain_error("missing system message"))?;
    match &message.content {
        MessageContent::Text(text) => Ok(text),
        MessageContent::Parts(_) => Err(plain_error("system content is not text")),
    }
}

pub(super) fn user_text(request: &ModelRequest) -> Result<&str, ModelProviderError> {
    let message = request
        .messages
        .get(1)
        .ok_or_else(|| plain_error("missing user message"))?;
    match &message.content {
        MessageContent::Text(text) => Ok(text),
        MessageContent::Parts(_) => Err(plain_error("user content is not text")),
    }
}

pub(super) fn kind_name(system: &str) -> &'static str {
    if system == LEAF_SYSTEM_PROMPT {
        "LEAF_SYSTEM_PROMPT"
    } else if system == BRANCH_SYSTEM_PROMPT {
        "BRANCH_SYSTEM_PROMPT"
    } else if system == ROOT_SYSTEM_PROMPT {
        "ROOT_SYSTEM_PROMPT"
    } else if system == TITLE_SYSTEM_PROMPT {
        "TITLE_SYSTEM_PROMPT"
    } else {
        "OTHER"
    }
}

pub(super) fn answer(kind: &str) -> String {
    match kind {
        "BRANCH_SYSTEM_PROMPT" => BRANCH_TEXT.to_owned(),
        "ROOT_SYSTEM_PROMPT" => ROOT_TEXT.to_owned(),
        "TITLE_SYSTEM_PROMPT" => TITLE_ANSWER.to_owned(),
        _ => LEAF_TEXT.to_owned(),
    }
}

pub(super) fn response(content: String, finish: Option<FinishReason>) -> LlmResponse {
    LlmResponse {
        r#type: LlmResponseKind::Text,
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls: None,
        usage: None,
        finish_reason: finish,
    }
}

pub(super) fn status_error(status: u16) -> ModelProviderError {
    let message = format!("model server returned {status} synthetic");
    ModelProviderError::Request {
        message,
        source: Box::new(std::io::Error::other("synthetic status")),
    }
}

pub(super) fn plain_error(message: &str) -> ModelProviderError {
    ModelProviderError::Request {
        message: message.to_owned(),
        source: Box::new(std::io::Error::other(message.to_owned())),
    }
}

impl Seen {
    pub(super) fn with_dead(dead: Vec<JobName>) -> Self {
        Self {
            jobs: Mutex::new(Vec::new()),
            dead: Mutex::new(dead),
            notifications: tokio::sync::watch::channel(()).0,
            next_id: std::sync::atomic::AtomicI64::new(1),
        }
    }

    pub(super) async fn only(&self) -> Job {
        let mut jobs = self.jobs.lock().await;
        jobs.retain(|seen| seen.job.name != JobName::Fixed(FixedJob::FileConversation));
        assert_eq!(jobs.len(), 1);
        jobs.remove(0).job
    }

    pub(super) async fn filing(&self) -> Vec<Job> {
        self.jobs
            .lock()
            .await
            .iter()
            .filter(|seen| seen.job.name == JobName::Fixed(FixedJob::FileConversation))
            .map(|seen| seen.job.clone())
            .collect()
    }

    pub(super) async fn dead_count(&self, job: FixedJob) -> usize {
        self.dead
            .lock()
            .await
            .iter()
            .filter(|name| **name == JobName::Fixed(job))
            .count()
    }
}

#[async_trait]
impl JobQueue for Seen {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        workers::jobs_for(role)
    }

    async fn subscribe(&self) -> Result<Box<dyn kernel::queue::JobNotifications>, JobQueueError> {
        Ok(Box::new(self.notifications.subscribe()))
    }

    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError> {
        let key = job.key.clone();
        let id = {
            let mut jobs = self.jobs.lock().await;
            if let Some(key) = key
                && let Some(existing) = jobs
                    .iter_mut()
                    .find(|queued| queued.job.key.as_ref() == Some(&key))
            {
                let id = existing.id;
                existing.job = job;
                id
            } else {
                let id = JobId::new(
                    self.next_id
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                );
                jobs.push(Held {
                    id,
                    job,
                    attempts: 0,
                    last_error: None,
                    claimed: false,
                });
                id
            }
        };
        self.notifications.send_replace(());
        Ok(id)
    }

    async fn claim(
        &self,
        selector: &kernel::queue::ClaimSelector,
    ) -> Result<Option<kernel::queue::ClaimedJob>, JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.iter_mut().find(|held| {
            !held.claimed
                && selector.names.iter().any(|name| name == &held.job.name)
                && selector.only_id.is_none_or(|only| only == held.id)
        }) {
            Some(held) => {
                held.claimed = true;
                held.attempts = held.attempts.saturating_add(1);
                Ok(Some(kernel::queue::ClaimedJob {
                    id: held.id,
                    name: held.job.name.clone(),
                    payload: held.job.payload.clone(),
                    attempt: held.attempts,
                }))
            }
            None => Ok(None),
        };
        drop(jobs);
        outcome
    }

    async fn complete(&self, id: JobId) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let before = jobs.len();
        jobs.retain(|held| held.id != id);
        let missing = jobs.len() == before;
        drop(jobs);
        if missing {
            Err(JobQueueError::Missing { id })
        } else {
            Ok(())
        }
    }

    async fn release(&self, id: JobId) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.iter_mut().find(|held| held.id == id) {
            Some(held) if held.claimed => {
                held.claimed = false;
                held.attempts = held.attempts.saturating_sub(1);
                Ok(())
            }
            Some(_) | None => Err(JobQueueError::Missing { id }),
        };
        drop(jobs);
        outcome
    }

    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError> {
        let mut jobs = self.jobs.lock().await;
        let outcome = match jobs.iter_mut().find(|held| held.id == id) {
            Some(held) => {
                held.last_error = Some(error.to_owned());
                held.claimed = false;
                Ok(())
            }
            None => Err(JobQueueError::Missing { id }),
        };
        drop(jobs);
        outcome
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        let flagged = std::mem::take(&mut *self.dead.lock().await).len();
        u64::try_from(flagged).map_err(|_| JobQueueError::Stored("flagged".to_owned()))
    }

    async fn read(&self, id: JobId) -> Result<Option<kernel::queue::StoredJob>, JobQueueError> {
        let jobs = self.jobs.lock().await;
        let stored = jobs
            .iter()
            .find(|held| held.id == id)
            .map(|held| {
                let max_attempts = match held.job.max_attempts {
                    Some(attempts) => attempts,
                    None => i32::try_from(default_attempts(&held.job.name))
                        .map_err(|_| JobQueueError::Stored("max_attempts".to_owned()))?,
                };
                Ok(kernel::queue::StoredJob {
                    id,
                    name: held.job.name.clone(),
                    payload: held.job.payload.clone(),
                    key: held.job.key.clone(),
                    priority: Some(held.job.priority),
                    max_attempts,
                    attempts: held.attempts,
                    last_error: held.last_error.clone(),
                })
            })
            .transpose();
        drop(jobs);
        stored
    }
}

fn default_attempts(name: &JobName) -> u32 {
    match name {
        JobName::Fixed(FixedJob::SummarizeConversation | FixedJob::SynthesizeToolCall) => 3,
        JobName::Fixed(FixedJob::FileConversation | FixedJob::ExtractWiki) => 2,
        JobName::Fixed(FixedJob::EmbedTarget) => 5,
        JobName::Heartbeat { .. } => 1,
        JobName::Fixed(_) => 25,
    }
}

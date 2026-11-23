mod chunks;
mod coverage;
mod ledger;
mod lifecycle;
mod pool;
mod sweep;
mod synthesis;
mod width;

pub use chunks::a_phrase_inside_a_long_message_outranks_its_parent;
pub use chunks::an_unembedded_chunk_is_not_healed_and_clears_once_written;
pub use chunks::chunks_are_written_once_and_an_unknown_message_is_skipped;
pub use chunks::unchunked_counts_trimmed_characters_and_caps_tool_output;
pub use coverage::a_recent_leaf_is_queued_ahead_of_an_old_one;
pub use coverage::dead_wiki_jobs_outside_the_windows_stay_put;
pub use ledger::a_dismissed_ledger_id_can_be_retried;
pub use ledger::a_re_added_key_is_flagged_on_the_next_failure;
pub use ledger::failure_times_move_forward_and_a_completed_retry_drops_its_count;
pub use ledger::ledger_job;
pub use ledger::replace_detaches_a_job_that_is_still_running;
pub use ledger::replace_resets_attempts_and_the_maximum;
pub use lifecycle::a_chunk_write_enqueues_its_embedding;
pub use lifecycle::an_exhausted_tool_synthesis_is_backfilled_again;
pub use lifecycle::an_exhausted_wiki_extraction_stays_suppressed;
pub use pool::the_nearest_chunk_pool_keeps_one_chunk_per_message;
pub use sweep::a_fresh_dead_wiki_job_is_not_revived;
pub use sweep::a_non_heartbeat_leaf_is_the_only_missing_wiki;
pub use sweep::a_tool_sentence_replaces_only_matching_content;
pub use sweep::an_empty_assistant_tool_call_is_backfilled_once;
pub use sweep::remember_tool_call;
pub use sweep::tool_arguments_are_rejected;
pub use sweep::tool_arguments_match;
pub use synthesis::a_tool_sentence_records_the_model_request;
pub use synthesis::synthesis_drops_missing_populated_and_nameless_calls;
pub use synthesis::synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one;
pub use width::a_chunk_write_rejects_a_vector_wider_than_the_message;

use kernel::memory::BrowseFilter;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::Limit;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryStore;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

const WIDTH: usize = 1024;

pub fn query_vector() -> Vec<f32> {
    axis(0, 1.0)
}

pub(super) fn axis(index: usize, value: f32) -> Vec<f32> {
    let mut vector = vec![0.0; WIDTH];
    if let Some(slot) = vector.get_mut(index) {
        *slot = value;
    }
    vector
}

pub(super) fn pair(first: f32, second: f32) -> Vec<f32> {
    let mut vector = axis(0, first);
    if let Some(slot) = vector.get_mut(1) {
        *slot = second;
    }
    vector
}

pub(super) struct Note<'a> {
    session: &'a str,
    role: Role,
    content: &'a str,
    tool_name: Option<&'a str>,
    tool_args: Option<JsonObject>,
    tool_result: Option<&'a str>,
}

pub(super) struct Saved {
    pub(super) conversation: Uuid,
    pub(super) message: Uuid,
}

pub(super) fn user<'a>(session: &'a str, content: &'a str) -> Note<'a> {
    noted(session, Role::User, content)
}

pub(super) fn noted<'a>(session: &'a str, role: Role, content: &'a str) -> Note<'a> {
    Note {
        session,
        role,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
    }
}

impl<'a> Note<'a> {
    pub(super) fn tool(
        mut self,
        name: &'a str,
        args: Option<JsonObject>,
        result: Option<&'a str>,
    ) -> Self {
        self.tool_name = Some(name);
        self.tool_args = args;
        self.tool_result = result;
        self
    }
}

pub(super) async fn message<S: MemoryStore>(
    store: &S,
    note: Note<'_>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(capture(store, note).await?.message)
}

pub(super) async fn capture<S: MemoryStore>(
    store: &S,
    note: Note<'_>,
) -> Result<Saved, Box<dyn std::error::Error>> {
    let at = wire::time::parse_rfc3339("2026-06-01T00:00:00Z")?;
    let owner = crate::owner()?;
    let captured = store
        .capture(
            CaptureBatchRequest {
                session_key: note.session.to_owned(),
                agent: "rivet".to_owned(),
                channel: Some("cli".to_owned()),
                title: None,
                settings: None,
                task_id: None,
                finalize: None,
                created_at: Some(at),
                updated_at: Some(at),
                messages: vec![CaptureMessage {
                    event_id: Uuid::new_v4().to_string(),
                    role: note.role,
                    content: note.content.to_owned(),
                    tool_name: note.tool_name.map(str::to_owned),
                    tool_args: note.tool_args,
                    tool_result: note.tool_result.map(str::to_owned),
                    metadata: None,
                    created_at: Some(at),
                }],
            },
            &owner,
            None,
        )
        .await?;
    let conversation = Uuid::parse_str(&captured.conversation_id)?;
    let rows = store.browse(browse(conversation, Some(note.role))).await?;
    let entry = rows
        .iter()
        .find(|row| row.content == note.content)
        .ok_or("the captured message is missing")?;
    let text = entry.id.clone().ok_or("the captured message has no id")?;
    Ok(Saved {
        conversation,
        message: Uuid::parse_str(&text)?,
    })
}

pub(super) fn browse(conversation: Uuid, role: Option<Role>) -> BrowseFilter {
    BrowseFilter {
        conversation_id: Some(conversation),
        agent: None,
        role,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: true,
        order: MemoryBrowseOrder::Desc,
        limit: Limit::clamped(Some(100), 50, 1..=200),
    }
}

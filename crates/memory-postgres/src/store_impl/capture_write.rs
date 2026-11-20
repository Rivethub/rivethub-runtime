use crate::capture::CaptureOptions;
use crate::capture::CaptureWriter;
use crate::capture::ConversationDates;
use crate::capture::ConversationUpsert;
use crate::capture::MessageRow;
use crate::capture::StagedMessages;
use crate::capture::UpsertedConversation;
use crate::capture::prepare_capture_message;
use crate::error::Error;
use kernel::memory::{CaptureBatchRequest, CaptureResult, spoken_title};
impl crate::PostgresMemory {
    pub async fn capture_batch(
        &self,
        batch: &CaptureBatchRequest,
        options: &CaptureOptions,
    ) -> Result<CaptureResult, Error> {
        match wire::capture::owned_import_key(options.owner.as_str(), &batch.session_key)? {
            Some(session_key) => {
                let owned = CaptureBatchRequest {
                    session_key,
                    ..batch.clone()
                };
                self.capture_keyed(&owned, options, Write::Capture).await
            }
            None => self.capture_keyed(batch, options, Write::Capture).await,
        }
    }

    pub(super) async fn capture_keyed(
        &self,
        batch: &CaptureBatchRequest,
        options: &CaptureOptions,
        write: Write,
    ) -> Result<CaptureResult, Error> {
        let channel = batch.channel.as_deref().ok_or(Error::MissingChannel)?;
        let mut writer = CaptureWriter::begin(&self.pool, &batch.session_key).await?;
        if self.missing.embeddings {
            writer.defer_embeddings().await?;
        }
        if write == Write::Append
            && writer
                .carries_receipt(&batch.session_key, options.owner.as_str())
                .await?
        {
            return Err(Error::ReceivedSession);
        }
        if let Some(dropped) = dropped_when_deleted(&mut writer, batch).await? {
            return Ok(dropped);
        }
        let observation = crate::settlement::capture_observation(
            &batch.session_key,
            batch.settings.as_ref(),
            batch.finalize,
        );
        if batch.messages.is_empty()
            && wire::capture::SessionObservation::is_session_end(
                &batch.session_key,
                batch.settings.as_ref(),
                batch.finalize,
            )
        {
            let existing = writer
                .existing_conversation(&batch.session_key, &batch.agent, options.owner.as_str())
                .await?;
            if existing.is_none() {
                return Ok(CaptureResult {
                    ok: true,
                    conversation_id: String::new(),
                    inserted: 0,
                    skipped: 0,
                });
            }
        }
        let prior = if observation.is_some() {
            writer
                .prior_summary_observation(&batch.session_key, &batch.agent)
                .await?
        } else {
            None
        };
        let conversation = upsert_batch_conversation(&mut writer, batch, channel, options).await?;
        let conversation_id = conversation.id;
        let (inserted, reordered) = write_capture_messages(
            &mut writer,
            batch,
            channel,
            &conversation,
            !self.missing.language,
        )
        .await?;
        let imported = wire::capture::is_imported_session(&batch.session_key);
        writer
            .record_summary_input(
                conversation_id,
                inserted > 0 || reordered,
                imported,
                observation.as_ref(),
                prior,
            )
            .await?;
        if batch.finalize == Some(true) {
            writer
                .finalize_conversation(conversation_id, imported)
                .await?;
        }
        writer.commit().await?;
        Ok(CaptureResult {
            ok: true,
            conversation_id: conversation_id.to_string(),
            inserted,
            skipped: u64::try_from(batch.messages.len())? - inserted,
        })
    }
}

async fn write_capture_messages(
    writer: &mut CaptureWriter<'_>,
    batch: &CaptureBatchRequest,
    channel: &str,
    conversation: &UpsertedConversation,
    language: bool,
) -> Result<(u64, bool), Error> {
    let conversation_id = conversation.id;
    let StagedMessages { fresh, positions } = writer
        .stage_messages(conversation_id, &batch.messages)
        .await?;
    let stamps = writer
        .untied_times(
            conversation_id,
            fresh
                .iter()
                .map(|(_, message)| message.created_at)
                .collect(),
        )
        .await?;
    let mut inserted: u64 = 0;
    for ((index, message), created_at) in fresh.into_iter().zip(stamps) {
        let prepared = prepare_capture_message(message)?;
        writer
            .insert_message(&MessageRow {
                conversation_id,
                agent: &batch.agent,
                channel,
                role: message.role,
                content: &prepared.content,
                tool_name: message.tool_name.as_deref(),
                tool_args: message.tool_args.as_ref(),
                tool_result: prepared.tool_result.as_deref(),
                metadata: &prepared.metadata,
                owner_user_id: &conversation.owner_user_id,
                created_at,
                batch_index: i64::try_from(index)?,
            })
            .await?;
        inserted += 1;
    }
    let reordered = if positions.is_empty() {
        false
    } else {
        writer
            .fill_positions(
                conversation_id,
                &conversation.owner_user_id,
                &positions,
                inserted == 0,
            )
            .await?
    };
    if inserted > 0 || !positions.is_empty() {
        writer.refresh_opening(conversation_id, language).await?;
    }
    Ok((inserted, reordered))
}

async fn upsert_batch_conversation(
    writer: &mut CaptureWriter<'_>,
    batch: &CaptureBatchRequest,
    channel: &str,
    options: &CaptureOptions,
) -> Result<UpsertedConversation, Error> {
    let imported = writer.imported_settings(batch).await?;
    let newer = imported
        .as_ref()
        .is_none_or(|merged| merged.incoming_is_newer);
    writer
        .upsert_conversation(&ConversationUpsert {
            session_key: &batch.session_key,
            agent: &batch.agent,
            channel: Some(channel),
            title: batch
                .title
                .as_deref()
                .and_then(spoken_title)
                .filter(|_| newer),
            settings: imported
                .as_ref()
                .map(|merged| &merged.settings)
                .or(batch.settings.as_ref()),
            task_id: batch.task_id.as_deref(),
            owner_user_id: options.owner.as_str(),
            node: options.node.as_deref(),
            dates: ConversationDates::of(batch),
        })
        .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Write {
    Capture,
    Append,
}

async fn dropped_when_deleted(
    writer: &mut CaptureWriter<'_>,
    batch: &CaptureBatchRequest,
) -> Result<Option<CaptureResult>, Error> {
    let Some(tombstone) = crate::deletion::tombstone_at(&mut writer.tx, &batch.session_key).await?
    else {
        return Ok(None);
    };
    let stamps = batch.messages.iter().map(|message| message.created_at);
    if kernel::memory::used_since(tombstone.deleted_at, stamps) {
        crate::deletion::lift(&mut writer.tx, &batch.session_key).await?;
        return Ok(None);
    }
    tracing::debug!(
        session_key = %batch.session_key,
        "capture dropped for a deleted session"
    );
    Ok(Some(CaptureResult {
        ok: true,
        conversation_id: String::new(),
        inserted: 0,
        skipped: u64::try_from(batch.messages.len())?,
    }))
}

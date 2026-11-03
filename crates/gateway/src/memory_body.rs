pub(crate) fn memory_chunk_match_body(
    value: kernel::memory::MemoryChunkMatch,
) -> wire::memory::MemoryChunkMatch {
    wire::memory::MemoryChunkMatch {
        index: value.index,
        total: value.total,
        char_start: value.char_start,
        char_end: value.char_end,
        content: value.content,
    }
}

pub(crate) const fn memory_hit_source_body(
    value: kernel::memory::MemoryHitSource,
) -> wire::gateway::memory::MemoryHitSource {
    match value {
        kernel::memory::MemoryHitSource::Message => wire::gateway::memory::MemoryHitSource::Message,
        kernel::memory::MemoryHitSource::Summary => wire::gateway::memory::MemoryHitSource::Summary,
    }
}

pub(crate) fn memory_degraded_body(
    value: kernel::memory::MemoryDegraded,
) -> wire::gateway::memory::MemoryDegraded {
    wire::gateway::memory::MemoryDegraded {
        reason: value.reason,
        effect: value.effect,
    }
}

pub(crate) const fn memory_fallback_body(
    value: kernel::memory::MemoryFallback,
) -> wire::gateway::memory::MemoryFallback {
    match value {
        kernel::memory::MemoryFallback::Trigram => wire::gateway::memory::MemoryFallback::Trigram,
    }
}

pub(crate) fn memory_entry_body(value: kernel::memory::MemoryEntry) -> wire::memory::MemoryEntry {
    wire::memory::MemoryEntry {
        id: value.id,
        session_id: value.session_id,
        agent: value.agent,
        channel: value.channel,
        role: value.role,
        content: value.content,
        tool_name: value.tool_name,
        tool_args: value.tool_args,
        tool_result: value.tool_result,
        metadata: value.metadata,
        created_at: value.created_at,
    }
}

pub(crate) fn memory_stats_response_body(
    value: kernel::memory::MemoryStatsResponse,
) -> wire::gateway::memory::MemoryStatsResponse {
    wire::gateway::memory::MemoryStatsResponse {
        conversations: value.conversations,
        messages: value.messages,
        tool_calls: value.tool_calls,
        summaries: value.summaries,
        embed_queue_depth: value.embed_queue_depth,
        embedded_messages: value.embedded_messages,
        failed_embeddings: value.failed_embeddings,
        top_tools: value.top_tools.into_iter().map(top_tool_body).collect(),
        recent_sessions: value
            .recent_sessions
            .into_iter()
            .map(recent_session_body)
            .collect(),
    }
}

pub(crate) fn top_tool_body(value: kernel::memory::TopTool) -> wire::gateway::memory::TopTool {
    wire::gateway::memory::TopTool {
        tool: value.tool,
        count: value.count,
    }
}

pub(crate) fn recent_session_body(
    value: kernel::memory::RecentSession,
) -> wire::gateway::memory::RecentSession {
    wire::gateway::memory::RecentSession {
        session_id: value.session_id,
        title: value.title,
        agent: value.agent,
        last_active: value.last_active,
        messages: value.messages,
    }
}

pub(crate) fn memory_summary_node_body(
    value: kernel::memory::MemorySummaryNode,
) -> wire::gateway::memory::MemorySummaryNode {
    wire::gateway::memory::MemorySummaryNode {
        id: value.id,
        parent_id: value.parent_id,
        level: value.level,
        kind: summary_kind_body(value.kind),
        content: value.content,
        message_count: value.message_count,
        earliest_at: value.earliest_at,
        latest_at: value.latest_at,
        created_at: value.created_at,
        source_message_ids: value.source_message_ids,
        vector: vector_state_body(value.vector),
        truncated: value.truncated,
        children: value.children,
    }
}

pub(crate) const fn summary_kind_body(
    value: kernel::memory::SummaryKind,
) -> wire::gateway::memory::SummaryKind {
    match value {
        kernel::memory::SummaryKind::Leaf => wire::gateway::memory::SummaryKind::Leaf,
        kernel::memory::SummaryKind::Branch => wire::gateway::memory::SummaryKind::Branch,
        kernel::memory::SummaryKind::Root => wire::gateway::memory::SummaryKind::Root,
    }
}

pub(crate) const fn vector_state_body(
    value: kernel::memory::VectorState,
) -> wire::gateway::memory::VectorState {
    match value {
        kernel::memory::VectorState::Embedded => wire::gateway::memory::VectorState::Embedded,
        kernel::memory::VectorState::Pending => wire::gateway::memory::VectorState::Pending,
        kernel::memory::VectorState::Failed => wire::gateway::memory::VectorState::Failed,
        kernel::memory::VectorState::Skipped => wire::gateway::memory::VectorState::Skipped,
    }
}

pub(crate) fn memory_message_tally_body(
    value: kernel::memory::MemoryMessageTally,
) -> wire::gateway::memory::MemoryMessageTally {
    wire::gateway::memory::MemoryMessageTally {
        total: value.total,
        user: value.user,
        assistant: value.assistant,
        system: value.system,
        tool: value.tool,
        embedded: value.embedded,
        pending: value.pending,
        failed: value.failed,
        skipped: value.skipped,
    }
}

pub(crate) fn tag_wire_body(value: kernel::memory::TagWire) -> wire::gateway::memory::TagWire {
    wire::gateway::memory::TagWire {
        id: value.id,
        entity_type: tag_entity_type_body(value.entity_type),
        entity_id: value.entity_id,
        key: value.key,
        value: value.value,
        display: value.display,
        source: value.source,
        state: tag_state_body(value.state),
        confidence: value.confidence,
        proposed_by: value.proposed_by,
        reason: value.reason,
        decided_by: value.decided_by,
        decided_at: value.decided_at,
        created_at: value.created_at,
        updated_at: value.updated_at,
    }
}

pub(crate) const fn tag_entity_type_body(
    value: kernel::memory::TagEntityType,
) -> wire::gateway::memory::TagEntityType {
    match value {
        kernel::memory::TagEntityType::Conversation => {
            wire::gateway::memory::TagEntityType::Conversation
        }
        kernel::memory::TagEntityType::Summary => wire::gateway::memory::TagEntityType::Summary,
    }
}

pub(crate) const fn tag_state_body(
    value: kernel::memory::TagState,
) -> wire::gateway::memory::TagState {
    match value {
        kernel::memory::TagState::Suggested => wire::gateway::memory::TagState::Suggested,
        kernel::memory::TagState::Accepted => wire::gateway::memory::TagState::Accepted,
        kernel::memory::TagState::Rejected => wire::gateway::memory::TagState::Rejected,
    }
}

pub(crate) fn pending_tag_wire_body(
    value: kernel::memory::PendingTagWire,
) -> wire::gateway::memory::PendingTagWire {
    wire::gateway::memory::PendingTagWire {
        tag: tag_wire_body(value.tag),
        action: value.action.map(pending_tag_action_body),
        session_key: value.session_key,
        title: value.title,
        agent: value.agent,
        conversation_id: value.conversation_id,
        excerpt: value.excerpt,
    }
}

pub(crate) const fn pending_tag_action_body(
    value: kernel::memory::PendingTagAction,
) -> wire::gateway::memory::PendingTagAction {
    match value {
        kernel::memory::PendingTagAction::Add => wire::gateway::memory::PendingTagAction::Add,
        kernel::memory::PendingTagAction::Remove => wire::gateway::memory::PendingTagAction::Remove,
    }
}

pub(crate) fn tag_count_body(value: kernel::memory::TagCount) -> wire::gateway::memory::TagCount {
    wire::gateway::memory::TagCount {
        key: value.key,
        value: value.value,
        display: value.display,
        conversations: value.conversations,
    }
}

pub(crate) fn tag_taxonomy_entry_body(
    value: kernel::memory::TagTaxonomyEntry,
) -> wire::gateway::memory::TagTaxonomyEntry {
    wire::gateway::memory::TagTaxonomyEntry {
        key: value.key,
        value: value.value,
        display: value.display,
        parent_value: value.parent_value,
        aliases: value.aliases,
        state: tag_state_body(value.state),
        source: value.source,
        reason: value.reason,
        decided_at: value.decided_at,
        created_at: value.created_at,
        updated_at: value.updated_at,
    }
}

pub(crate) fn tag_taxonomy_merge_result_body(
    value: kernel::memory::TagTaxonomyMergeResult,
) -> wire::gateway::memory::TagTaxonomyMergeResult {
    wire::gateway::memory::TagTaxonomyMergeResult {
        moved: value.moved,
        dropped: value.dropped,
        into: value.into,
    }
}

pub(crate) fn wiki_topic_list_body(
    value: kernel::wiki::WikiTopicList,
) -> wire::gateway::wiki::WikiTopicList {
    wire::gateway::wiki::WikiTopicList {
        topics: value.topics.into_iter().map(wiki_topic_body).collect(),
        total: value.total,
    }
}

pub(crate) fn wiki_topic_body(value: kernel::wiki::WikiTopic) -> wire::gateway::wiki::WikiTopic {
    wire::gateway::wiki::WikiTopic {
        slug: value.slug,
        title: value.title,
        tags: value.tags,
        entities: value.entities,
        updated_at: value.updated_at,
        excerpt: value.excerpt,
        topic_id: value.topic_id,
    }
}

pub(crate) fn wiki_gaps_response_body(
    value: kernel::wiki::WikiGapsResponse,
) -> wire::gateway::wiki::WikiGapsResponse {
    wire::gateway::wiki::WikiGapsResponse {
        red_links: value
            .red_links
            .into_iter()
            .map(wiki_red_link_body)
            .collect(),
        stalest: value.stalest.into_iter().map(wiki_topic_body).collect(),
    }
}

pub(crate) fn wiki_red_link_body(
    value: kernel::wiki::WikiRedLink,
) -> wire::gateway::wiki::WikiRedLink {
    wire::gateway::wiki::WikiRedLink {
        entity: value.entity,
        referenced_by: value.referenced_by,
    }
}

pub(crate) fn wiki_page_response_body(
    value: kernel::wiki::WikiPageResponse,
) -> wire::gateway::wiki::WikiPageResponse {
    wire::gateway::wiki::WikiPageResponse {
        slug: value.slug,
        title: value.title,
        aliases: value.aliases,
        tags: value.tags,
        entities: value.entities,
        current_state: value.current_state,
        article: value.article,
        see_also: value.see_also,
        history: value
            .history
            .into_iter()
            .map(wiki_history_entry_body)
            .collect(),
        citations: value
            .citations
            .into_iter()
            .map(wiki_citation_body)
            .collect(),
        markdown: value.markdown,
        sources: value.sources.into_iter().map(wiki_source_body).collect(),
        git_sha: value.git_sha,
        last_verified: value.last_verified,
        updated_at: value.updated_at,
        related: value.related,
        topic_id: value.topic_id,
    }
}

pub(crate) fn wiki_citation_body(
    value: kernel::wiki::WikiCitation,
) -> wire::gateway::wiki::WikiCitation {
    wire::gateway::wiki::WikiCitation {
        summary_id: value.summary_id,
        date: value.date,
        kind: value.kind,
        note: value.note,
        conversation_id: value.conversation_id,
        session_id: value.session_id,
        title: value.title,
        channel: value.channel,
        topic_id: value.topic_id,
    }
}

pub(crate) fn wiki_source_body(value: kernel::wiki::WikiSource) -> wire::gateway::wiki::WikiSource {
    wire::gateway::wiki::WikiSource {
        kind: wiki_source_kind_body(value.kind),
        ids: value.ids,
        conversation_id: value.conversation_id,
        span: value.span.map(wiki_source_span_body),
    }
}

pub(crate) fn wiki_source_span_body(
    value: kernel::wiki::WikiSourceSpan,
) -> wire::gateway::wiki::WikiSourceSpan {
    wire::gateway::wiki::WikiSourceSpan {
        earliest: value.earliest,
        latest: value.latest,
    }
}

pub(crate) fn wiki_history_entry_body(
    value: kernel::wiki::page::WikiHistoryEntry,
) -> wire::gateway::wiki::WikiHistoryEntry {
    wire::gateway::wiki::WikiHistoryEntry {
        date: value.date,
        title: value.title,
        body: value.body,
    }
}

pub(crate) const fn wiki_source_kind_body(
    value: kernel::wiki::page::WikiSourceKind,
) -> wire::gateway::wiki::WikiSourceKind {
    match value {
        kernel::wiki::page::WikiSourceKind::Summary => wire::gateway::wiki::WikiSourceKind::Summary,
        kernel::wiki::page::WikiSourceKind::Message => wire::gateway::wiki::WikiSourceKind::Message,
        kernel::wiki::page::WikiSourceKind::Conversation => {
            wire::gateway::wiki::WikiSourceKind::Conversation
        }
        kernel::wiki::page::WikiSourceKind::Task => wire::gateway::wiki::WikiSourceKind::Task,
        kernel::wiki::page::WikiSourceKind::Leaf => wire::gateway::wiki::WikiSourceKind::Leaf,
        kernel::wiki::page::WikiSourceKind::ExtractWiki => {
            wire::gateway::wiki::WikiSourceKind::ExtractWiki
        }
        kernel::wiki::page::WikiSourceKind::User => wire::gateway::wiki::WikiSourceKind::User,
    }
}

pub(crate) const fn search_mode(
    value: wire::gateway::memory::SearchMode,
) -> kernel::memory::SearchMode {
    match value {
        wire::gateway::memory::SearchMode::Hybrid => kernel::memory::SearchMode::Hybrid,
        wire::gateway::memory::SearchMode::Fts => kernel::memory::SearchMode::Fts,
        wire::gateway::memory::SearchMode::Vector => kernel::memory::SearchMode::Vector,
        wire::gateway::memory::SearchMode::Trigram => kernel::memory::SearchMode::Trigram,
        wire::gateway::memory::SearchMode::Regex => kernel::memory::SearchMode::Regex,
    }
}

pub(crate) const fn memory_scope(
    value: wire::gateway::memory::MemoryScope,
) -> kernel::memory::MemoryScope {
    match value {
        wire::gateway::memory::MemoryScope::Messages => kernel::memory::MemoryScope::Messages,
        wire::gateway::memory::MemoryScope::Summaries => kernel::memory::MemoryScope::Summaries,
        wire::gateway::memory::MemoryScope::Both => kernel::memory::MemoryScope::Both,
    }
}

pub(crate) const fn browse_window(
    value: wire::gateway::memory::BrowseWindow,
) -> kernel::memory::BrowseWindow {
    match value {
        wire::gateway::memory::BrowseWindow::Today => kernel::memory::BrowseWindow::Today,
        wire::gateway::memory::BrowseWindow::Yesterday => kernel::memory::BrowseWindow::Yesterday,
        wire::gateway::memory::BrowseWindow::ThisMorning => {
            kernel::memory::BrowseWindow::ThisMorning
        }
        wire::gateway::memory::BrowseWindow::ThisWeek => kernel::memory::BrowseWindow::ThisWeek,
        wire::gateway::memory::BrowseWindow::Last24h => kernel::memory::BrowseWindow::Last24h,
        wire::gateway::memory::BrowseWindow::Last7d => kernel::memory::BrowseWindow::Last7d,
        wire::gateway::memory::BrowseWindow::Last14d => kernel::memory::BrowseWindow::Last14d,
    }
}

pub(crate) const fn memory_browse_order(
    value: wire::gateway::memory::MemoryBrowseOrder,
) -> kernel::memory::MemoryBrowseOrder {
    match value {
        wire::gateway::memory::MemoryBrowseOrder::Asc => kernel::memory::MemoryBrowseOrder::Asc,
        wire::gateway::memory::MemoryBrowseOrder::Desc => kernel::memory::MemoryBrowseOrder::Desc,
    }
}

pub(crate) const fn tag_state(value: wire::gateway::memory::TagState) -> kernel::memory::TagState {
    match value {
        wire::gateway::memory::TagState::Suggested => kernel::memory::TagState::Suggested,
        wire::gateway::memory::TagState::Accepted => kernel::memory::TagState::Accepted,
        wire::gateway::memory::TagState::Rejected => kernel::memory::TagState::Rejected,
    }
}

pub(crate) const fn tag_entity_type(
    value: wire::gateway::memory::TagEntityType,
) -> kernel::memory::TagEntityType {
    match value {
        wire::gateway::memory::TagEntityType::Conversation => {
            kernel::memory::TagEntityType::Conversation
        }
        wire::gateway::memory::TagEntityType::Summary => kernel::memory::TagEntityType::Summary,
    }
}

pub(crate) fn tag_entry_pair(
    value: wire::gateway::memory::TagEntryPair,
) -> kernel::memory::TagEntryPair {
    kernel::memory::TagEntryPair {
        key: value.key,
        value: value.value,
    }
}

pub(crate) fn capture_batch_request(
    value: wire::capture::CaptureBatchRequest,
) -> kernel::memory::CaptureBatchRequest {
    kernel::memory::CaptureBatchRequest {
        session_key: value.session_key,
        agent: value.agent,
        channel: value.channel,
        title: value.title,
        settings: value.settings,
        task_id: value.task_id,
        finalize: value.finalize,
        created_at: value.created_at,
        updated_at: value.updated_at,
        messages: value.messages.into_iter().map(capture_message).collect(),
    }
}

pub(crate) fn capture_message(
    value: wire::capture::CaptureMessage,
) -> kernel::memory::CaptureMessage {
    kernel::memory::CaptureMessage {
        event_id: value.event_id,
        role: value.role,
        content: value.content,
        tool_name: value.tool_name,
        tool_args: value.tool_args,
        tool_result: value.tool_result,
        metadata: value.metadata,
        created_at: value.created_at,
    }
}

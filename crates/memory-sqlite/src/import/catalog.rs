use super::ImportTable;

pub(super) fn columns(table: ImportTable) -> Option<&'static [&'static str]> {
    match table {
        ImportTable::Conversations => Some(conversations()),
        ImportTable::Messages => Some(messages()),
        ImportTable::Summaries => Some(summaries()),
        ImportTable::SummarySources => Some(summary_sources()),
        ImportTable::SummaryDeliveries => Some(&[
            "conversation_id",
            "consumer",
            "message_id",
            "anchor_id",
            "completed_version",
            "historical_backlog",
        ]),
        ImportTable::Tags => Some(tags()),
        ImportTable::TagTaxonomy => Some(tag_taxonomy()),
        ImportTable::WikiTopics => Some(wiki_topics()),
        ImportTable::WikiProvenance => Some(wiki_provenance()),
        ImportTable::WikiExtractions => Some(wiki_extractions()),
        ImportTable::WikiCitations => Some(wiki_citations()),
        ImportTable::WikiRedirects => Some(wiki_redirects()),
        ImportTable::Jobs | ImportTable::Meta | ImportTable::EmbedQueue => None,
    }
}

const ORDER_SUMMARIES: &str = "depth DESC, id";
const ORDER_SUMMARY_SOURCES: &str = "summary_id, ordinal, message_id";
const ORDER_TAXONOMY: &str = "key, value";
const ORDER_TOPICS: &str = "slug";
const ORDER_PROVENANCE: &str = "topic_slug, source_kind, source_id";
const ORDER_EXTRACTIONS: &str = "summary_id";
const ORDER_CITATIONS: &str = "topic_slug, summary_id";
const ORDER_REDIRECTS: &str = "from_slug";
const ORDER_ID: &str = "id";

pub(super) fn order(table: ImportTable) -> &'static str {
    match table {
        ImportTable::Summaries => ORDER_SUMMARIES,
        ImportTable::SummarySources => ORDER_SUMMARY_SOURCES,
        ImportTable::SummaryDeliveries => "conversation_id, consumer, message_id",
        ImportTable::TagTaxonomy => ORDER_TAXONOMY,
        ImportTable::WikiTopics => ORDER_TOPICS,
        ImportTable::WikiProvenance => ORDER_PROVENANCE,
        ImportTable::WikiExtractions => ORDER_EXTRACTIONS,
        ImportTable::WikiCitations => ORDER_CITATIONS,
        ImportTable::WikiRedirects => ORDER_REDIRECTS,
        ImportTable::Conversations
        | ImportTable::Messages
        | ImportTable::Tags
        | ImportTable::Jobs
        | ImportTable::Meta
        | ImportTable::EmbedQueue => ORDER_ID,
    }
}

fn conversations() -> &'static [&'static str] {
    &[
        "id",
        "session_key",
        "agent",
        "channel",
        "channel_id",
        "bot_identity",
        "title",
        "settings",
        "active",
        "created_at",
        "updated_at",
        "task_id",
        "owner_user_id",
    ]
}

fn messages() -> &'static [&'static str] {
    &[
        "id",
        "conversation_id",
        "agent",
        "channel",
        "role",
        "content",
        "tool_name",
        "tool_args",
        "tool_result",
        "metadata",
        "embedding",
        "access_count",
        "last_accessed_at",
        "created_at",
        "embed_failures",
        "embed_error",
        "embed_status",
        "owner_user_id",
        "content_hash",
    ]
}

fn summaries() -> &'static [&'static str] {
    &[
        "id",
        "conversation_id",
        "parent_id",
        "depth",
        "content",
        "kind",
        "message_count",
        "earliest_at",
        "latest_at",
        "embedding",
        "model",
        "access_count",
        "last_accessed_at",
        "created_at",
        "embed_failures",
        "embed_error",
        "embed_status",
        "pipeline_version",
        "summary_ordinal",
        "issued_part_ordinal",
        "input_token_estimate",
        "source_first",
        "source_last",
        "range_verified",
        "input_key",
        "index_key",
        "quality_flags",
        "section_char_start",
        "section_char_end",
        "index_needs_repair",
        "owner_user_id",
    ]
}

fn summary_sources() -> &'static [&'static str] {
    &["summary_id", "message_id", "ordinal", "render_key"]
}

fn tags() -> &'static [&'static str] {
    &[
        "id",
        "entity_type",
        "entity_id",
        "key",
        "value",
        "display",
        "source",
        "state",
        "confidence",
        "proposed_by",
        "reason",
        "decided_by",
        "decided_at",
        "created_at",
        "updated_at",
        "removal_state",
        "removal_reason",
    ]
}

fn tag_taxonomy() -> &'static [&'static str] {
    &[
        "key",
        "value",
        "display",
        "parent_value",
        "aliases",
        "state",
        "source",
        "reason",
        "decided_at",
        "created_at",
        "updated_at",
    ]
}

fn wiki_topics() -> &'static [&'static str] {
    &[
        "slug",
        "title",
        "aliases",
        "tags",
        "entities",
        "current_state",
        "search_text",
        "embedding",
        "embed_status",
        "embed_failures",
        "embed_error",
        "history_count",
        "git_sha",
        "last_verified_at",
        "created_at",
        "updated_at",
        "article",
        "related",
    ]
}

fn wiki_provenance() -> &'static [&'static str] {
    &[
        "topic_slug",
        "source_kind",
        "source_id",
        "conversation_id",
        "git_sha",
        "created_at",
    ]
}

fn wiki_extractions() -> &'static [&'static str] {
    &[
        "summary_id",
        "status",
        "pipeline_version",
        "topics_touched",
        "git_sha",
        "error",
        "extracted_at",
    ]
}

fn wiki_citations() -> &'static [&'static str] {
    &["topic_slug", "summary_id", "kind", "note", "cited_at"]
}

fn wiki_redirects() -> &'static [&'static str] {
    &["from_slug", "to_slug", "created_at"]
}

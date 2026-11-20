use crate::import::ImportTable;

pub(super) fn predicate(table: ImportTable, mark: &str) -> String {
    match table {
        ImportTable::Conversations => conversations(mark),
        ImportTable::Messages => messages(mark),
        ImportTable::Summaries => summaries(mark),
        ImportTable::SummarySources => summary_sources(mark),
        ImportTable::SummaryDeliveries => deliveries(mark),
        ImportTable::Tags => tags(mark),
        ImportTable::TagTaxonomy => taxonomy(mark),
        ImportTable::WikiTopics => topics(mark),
        ImportTable::WikiProvenance | ImportTable::WikiRedirects => {
            format!("created_at >= {mark}")
        }
        ImportTable::WikiExtractions => extractions(mark),
        ImportTable::WikiCitations => citations(mark),
        ImportTable::Jobs | ImportTable::Meta | ImportTable::EmbedQueue => String::from("1 = 0"),
    }
}

fn summary_touch(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR earliest_at >= {mark} OR latest_at >= {mark} \
         OR id IN (SELECT summary_id FROM ros_wiki_extractions WHERE extracted_at >= {mark}) \
         OR (pipeline_version = 7 AND id IN (SELECT summary_id FROM ros_wiki_citations WHERE cited_at >= {mark}))"
    )
}

fn summaries(mark: &str) -> String {
    format!(
        "({touch}) OR (pipeline_version = 7 AND conversation_id IN (\
            SELECT conversation_id FROM ros_summaries WHERE pipeline_version = 7 AND ({touch}))) \
         OR id IN (\
            WITH RECURSIVE chain(id) AS (\
                SELECT parent_id FROM ros_summaries \
                 WHERE parent_id IS NOT NULL AND ({touch}) \
                UNION \
                SELECT s.parent_id FROM ros_summaries s \
                JOIN chain c ON s.id = c.id \
                 WHERE s.parent_id IS NOT NULL\
            ) SELECT id FROM chain)",
        touch = summary_touch(mark)
    )
}

fn messages(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR id IN (\
            SELECT message_id FROM ros_summary_sources \
             WHERE summary_id IN (SELECT id FROM ros_summaries WHERE {summaries})) \
         OR id IN (SELECT message_id FROM ros_summary_deliveries WHERE {deliveries})",
        summaries = summaries(mark),
        deliveries = deliveries(mark)
    )
}

fn conversations(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} \
         OR id IN (SELECT conversation_id FROM ros_messages WHERE {messages}) \
         OR id IN (SELECT conversation_id FROM ros_summaries \
                    WHERE {summaries} AND conversation_id IS NOT NULL) \
         OR id IN (SELECT conversation_id FROM ros_wiki_provenance \
                    WHERE created_at >= {mark} AND conversation_id IS NOT NULL) \
         OR id IN (SELECT entity_id FROM ros_tags \
                    WHERE entity_type = 'conversation' \
                      AND (created_at >= {mark} OR updated_at >= {mark}))",
        messages = messages(mark),
        summaries = summaries(mark)
    )
}

fn summary_sources(mark: &str) -> String {
    format!(
        "summary_id IN (SELECT id FROM ros_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

fn deliveries(mark: &str) -> String {
    format!(
        "conversation_id IN (SELECT conversation_id FROM ros_summaries
                             WHERE pipeline_version = 7 AND ({}))",
        summaries(mark)
    )
}

fn tags(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} \
         OR (entity_type = 'conversation' AND entity_id IN (\
                SELECT id FROM ros_conversations \
                 WHERE created_at >= {mark} OR updated_at >= {mark})) \
         OR (entity_type = 'summary' AND entity_id IN (\
                SELECT id FROM ros_summaries WHERE {summaries}))",
        summaries = summaries(mark)
    )
}

fn taxonomy(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} OR decided_at >= {mark} \
         OR (key, value) IN (SELECT key, value FROM ros_tags WHERE {tags})",
        tags = tags(mark)
    )
}

fn topics(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} OR last_verified_at >= {mark} \
         OR slug IN (SELECT topic_slug FROM ros_wiki_citations WHERE cited_at >= {mark}) \
         OR slug IN (SELECT topic_slug FROM ros_wiki_provenance WHERE created_at >= {mark}) \
         OR slug IN (SELECT to_slug FROM ros_wiki_redirects WHERE created_at >= {mark}) \
         OR slug IN (SELECT topic_slug FROM ros_wiki_citations \
                      WHERE summary_id IN (SELECT id FROM ros_summaries WHERE {summaries}))",
        summaries = summaries(mark)
    )
}

fn extractions(mark: &str) -> String {
    format!(
        "extracted_at >= {mark} OR summary_id IN (SELECT id FROM ros_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

fn citations(mark: &str) -> String {
    format!(
        "cited_at >= {mark} OR summary_id IN (SELECT id FROM ros_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

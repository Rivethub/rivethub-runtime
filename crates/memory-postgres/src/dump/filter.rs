use crate::error::Error;

pub(super) fn predicate(relation: &str, mark: &str) -> Result<String, Error> {
    let body = match relation {
        "rh_conversations" => conversations(mark),
        "rh_messages" => messages(mark),
        "rh_summaries" => summaries(mark),
        "rh_summary_sources" => summary_sources(mark),
        "rh_summary_passages" => format!(
            "record_id IN (SELECT id FROM rh_summaries WHERE {})",
            summaries(mark)
        ),
        "rh_summary_deliveries" => deliveries(mark),
        "rh_tags" => tags(mark),
        "rh_tag_taxonomy" => taxonomy(mark),
        "rh_wiki_topics" => topics(mark),
        "rh_wiki_provenance" | "rh_wiki_redirects" => format!("created_at >= {mark}"),
        "rh_wiki_extractions" => extractions(mark),
        "rh_wiki_citations" => citations(mark),
        _ => {
            return Err(Error::DumpMessage(format!(
                "unknown memory relation {relation}"
            )));
        }
    };
    Ok(body)
}

pub(super) fn order(relation: &str) -> Result<&'static str, Error> {
    let order = match relation {
        "rh_conversations" | "rh_messages" | "rh_tags" => "id",
        "rh_summaries" => "depth DESC, id",
        "rh_summary_sources" => "summary_id, ordinal, message_id",
        "rh_summary_passages" => "record_id, ordinal",
        "rh_summary_deliveries" => "conversation_id, consumer, message_id",
        "rh_tag_taxonomy" => "key, value",
        "rh_wiki_topics" => "slug",
        "rh_wiki_provenance" => "topic_slug, source_kind, source_id",
        "rh_wiki_extractions" => "summary_id",
        "rh_wiki_citations" => "topic_slug, summary_id",
        "rh_wiki_redirects" => "from_slug",
        _ => {
            return Err(Error::DumpMessage(format!(
                "unknown memory relation {relation}"
            )));
        }
    };
    Ok(order)
}

fn summary_touch(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR earliest_at >= {mark} OR latest_at >= {mark} \
         OR id IN (SELECT summary_id FROM rh_wiki_extractions WHERE extracted_at >= {mark}) \
         OR (pipeline_version = 7 AND id IN (SELECT summary_id FROM rh_wiki_citations WHERE cited_at >= {mark}))"
    )
}

fn summaries(mark: &str) -> String {
    format!(
        "({touch}) OR (pipeline_version = 7 AND conversation_id IN (\
            SELECT conversation_id FROM rh_summaries WHERE pipeline_version = 7 AND ({touch}))) \
         OR id IN (\
            WITH RECURSIVE chain(id) AS (\
                SELECT parent_id FROM rh_summaries \
                 WHERE parent_id IS NOT NULL AND ({touch}) \
                UNION \
                SELECT s.parent_id FROM rh_summaries s \
                JOIN chain c ON s.id = c.id \
                 WHERE s.parent_id IS NOT NULL\
            ) SELECT id FROM chain)",
        touch = summary_touch(mark)
    )
}

fn messages(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR id IN (\
            SELECT message_id FROM rh_summary_sources \
             WHERE summary_id IN (SELECT id FROM rh_summaries WHERE {summaries})) \
         OR id IN (SELECT message_id FROM rh_summary_deliveries WHERE {deliveries})",
        summaries = summaries(mark),
        deliveries = deliveries(mark)
    )
}

fn conversations(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} \
         OR id IN (SELECT conversation_id FROM rh_messages WHERE {messages}) \
         OR id IN (SELECT conversation_id FROM rh_summaries \
                    WHERE {summaries} AND conversation_id IS NOT NULL) \
         OR id IN (SELECT conversation_id FROM rh_wiki_provenance \
                    WHERE created_at >= {mark} AND conversation_id IS NOT NULL) \
         OR id IN (SELECT entity_id FROM rh_tags \
                    WHERE entity_type = 'conversation' \
                      AND (created_at >= {mark} OR updated_at >= {mark}))",
        messages = messages(mark),
        summaries = summaries(mark)
    )
}

fn summary_sources(mark: &str) -> String {
    format!(
        "summary_id IN (SELECT id FROM rh_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

fn deliveries(mark: &str) -> String {
    format!(
        "conversation_id IN (SELECT conversation_id FROM rh_summaries
                             WHERE pipeline_version = 7 AND ({}))",
        summaries(mark)
    )
}

fn tags(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} \
         OR (entity_type = 'conversation' AND entity_id IN (\
                SELECT id FROM rh_conversations \
                 WHERE created_at >= {mark} OR updated_at >= {mark})) \
         OR (entity_type = 'summary' AND entity_id IN (\
                SELECT id FROM rh_summaries WHERE {summaries}))",
        summaries = summaries(mark)
    )
}

fn taxonomy(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} OR decided_at >= {mark} \
         OR (key, value) IN (SELECT key, value FROM rh_tags WHERE {tags})",
        tags = tags(mark)
    )
}

fn topics(mark: &str) -> String {
    format!(
        "created_at >= {mark} OR updated_at >= {mark} OR last_verified_at >= {mark} \
         OR slug IN (SELECT topic_slug FROM rh_wiki_citations WHERE cited_at >= {mark}) \
         OR slug IN (SELECT topic_slug FROM rh_wiki_provenance WHERE created_at >= {mark}) \
         OR slug IN (SELECT to_slug FROM rh_wiki_redirects WHERE created_at >= {mark}) \
         OR slug IN (SELECT topic_slug FROM rh_wiki_citations \
                      WHERE summary_id IN (SELECT id FROM rh_summaries WHERE {summaries}))",
        summaries = summaries(mark)
    )
}

fn extractions(mark: &str) -> String {
    format!(
        "extracted_at >= {mark} OR summary_id IN (SELECT id FROM rh_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

fn citations(mark: &str) -> String {
    format!(
        "cited_at >= {mark} OR summary_id IN (SELECT id FROM rh_summaries WHERE {summaries})",
        summaries = summaries(mark)
    )
}

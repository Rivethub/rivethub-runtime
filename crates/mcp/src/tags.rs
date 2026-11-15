use crate::catalog::ToolInput as _;
use crate::{Error, error::required, memory::limit, render_tags, text};
use kernel::memory::{MemoryStore, TagCountsQuery, TagListQuery, TagLookup, TagTaxonomyQuery};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use uuid::Uuid;
use wire::gateway::memory::{TagEntityType, TagState};

#[derive(schemars::JsonSchema, serde::Serialize, Debug, Default, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
enum Action {
    List,
    #[default]
    Pending,
    Counts,
    Lookup,
    Taxonomy,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[schemars(!deny_unknown_fields)]
#[serde(deny_unknown_fields)]
pub struct Args {
    #[serde(default)]
    #[schemars(description = "What to do (default: pending)")]
    action: Action,
    #[schemars(description = "Conversation or summary id (list)", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    entity_id: Option<Uuid>,
    #[schemars(description = "list", transform = crate::catalog::optional_non_null)]
    entity_type: Option<TagEntityType>,
    #[schemars(description = "Tag key, e.g. project, topic", transform = crate::catalog::optional_non_null)]
    key: Option<String>,
    #[schemars(range(min = 1, max = 1000), transform = crate::catalog::optional_non_null)]
    limit: Option<u32>,
    #[schemars(description = "lookup", length(max = 500), transform = crate::catalog::optional_non_null)]
    session_keys: Option<Vec<String>>,
    #[schemars(description = "list / lookup / taxonomy: filter", transform = crate::catalog::optional_non_null)]
    state: Option<TagState>,
    #[schemars(description = "Tag value", transform = crate::catalog::optional_non_null)]
    value: Option<String>,
}

impl crate::catalog::ToolInput for Args {
    const NAME: &'static str = "memory_tags";
    const DESCRIPTION: &'static str = "Read key:value tags on sessions and summaries (session tagging). Actions: pending (suggestions awaiting review; the default), list (tags on an entity or by key+value), counts (accepted usage per tag), lookup (tags for session_keys), taxonomy (the vocabulary). This surface is read-only: review and edit tags in the hub under Memory → Tags. Use tag=key:value on memory_search / memory_browse to filter by an accepted tag.";
    fn annotations() -> Option<rmcp::model::ToolAnnotations> {
        Some(rmcp::model::ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

pub async fn call(store: &dyn MemoryStore, args: Args) -> Result<CallToolResult, Error> {
    let count = limit(Args::NAME, args.limit, 100, 1000)?;
    match args.action {
        Action::Pending => Ok(text(render_tags::pending(
            &store.pending_tags(count).await?,
        )?)),
        Action::List => Ok(text(render_tags::list(
            &store
                .list_tags(TagListQuery {
                    entity_type: args.entity_type.map(entity_type),
                    entity_id: args.entity_id,
                    key: args.key,
                    value: args.value,
                    states: args.state.into_iter().map(tag_state).collect(),
                    limit: count,
                })
                .await?,
        ))),
        Action::Counts => Ok(text(render_tags::counts(
            &store
                .tag_counts(TagCountsQuery {
                    key: args.key,
                    limit: count,
                })
                .await?,
        ))),
        Action::Lookup => {
            let session_keys = required(args.session_keys, "session_keys")?;
            bounded(&session_keys, 500)?;
            let sessions = store
                .lookup_tags(TagLookup {
                    session_keys: session_keys.clone(),
                    states: args.state.into_iter().map(tag_state).collect(),
                })
                .await?;
            Ok(text(render_tags::lookup(&session_keys, &sessions)))
        }
        Action::Taxonomy => Ok(text(render_tags::taxonomy(
            &store
                .list_tag_taxonomy(TagTaxonomyQuery {
                    key: args.key,
                    state: args.state.map(tag_state),
                    limit: count,
                })
                .await?,
        ))),
    }
}

fn bounded<T>(values: &[T], maximum: usize) -> Result<(), Error> {
    if values.is_empty() || values.len() > maximum {
        return Err(Error::Invalid(format!("expected 1..={maximum} items")));
    }
    Ok(())
}

const fn entity_type(entity: TagEntityType) -> kernel::memory::TagEntityType {
    match entity {
        TagEntityType::Conversation => kernel::memory::TagEntityType::Conversation,
        TagEntityType::Summary => kernel::memory::TagEntityType::Summary,
    }
}

const fn tag_state(state: TagState) -> kernel::memory::TagState {
    match state {
        TagState::Suggested => kernel::memory::TagState::Suggested,
        TagState::Accepted => kernel::memory::TagState::Accepted,
        TagState::Rejected => kernel::memory::TagState::Rejected,
    }
}

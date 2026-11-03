use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::routing;
use chrono::Utc;
use kernel::topics::PageEdit;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicPageError;
use kernel::topics::TopicStore;
use kernel::topics::TopicTree;
use kernel::topics::bound_page;
use kernel::topics::edit_topic_page;
use kernel::wiki::builder::WikiStoreError;
use wire::gateway::topics::TopicMention;
use wire::gateway::topics::TopicPage;
use wire::gateway::wiki::WikiPageEdit;

use super::map;
use super::resolve;
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;

const MENTIONS_SHOWN: u32 = 24;

pub(super) fn router() -> Router<Arc<Gateway>> {
    Router::new().route(
        "/api/topics/{topic}/page",
        routing::get(page)
            .put(edit)
            .fallback(super::super::method_not_allowed),
    )
}

pub(super) async fn page(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<Json<TopicPage>, ApiError> {
    let topic = resolve(gateway.topics()?.as_ref(), &key).await?;
    answer(&gateway, topic).await
}

pub(super) async fn edit(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    Body(request): Body<WikiPageEdit>,
) -> Result<Json<TopicPage>, ApiError> {
    let store = gateway.topics()?;
    let topic = resolve(store.as_ref(), &key).await?;
    let edit = PageEdit {
        current_state: request.current_state,
        article: request.article,
    };
    let edited = edit_topic_page(store.as_ref(), gateway.pages()?, &topic, &edit, Utc::now())
        .await
        .map_err(refused)?;
    if edited.is_none() {
        return Err(ApiError::Conflict("A type has no page".to_owned()));
    }
    answer(&gateway, topic).await
}

async fn answer(gateway: &Gateway, topic: Topic) -> Result<Json<TopicPage>, ApiError> {
    let store = gateway.topics()?;
    let page = bound_page(store.as_ref(), gateway.store().as_ref(), &topic)
        .await
        .map_err(map::page_error)?;
    let mentions = match topic.kind {
        TopicKind::Work => mentions(gateway, store.as_ref(), &topic).await?,
        TopicKind::Type | TopicKind::Entity => Vec::new(),
    };
    let subtopic = topic.kind == TopicKind::Work && topic.parent.is_some();
    let page = page.map(|page| {
        if subtopic {
            page.section_view(&topic.id.to_string(), topic.name.as_str())
        } else {
            page
        }
    });
    Ok(Json(TopicPage {
        topic: map::record(topic),
        page: page.map(crate::memory_body::wiki_page_response_body),
        mentions,
    }))
}

async fn mentions(
    gateway: &Gateway,
    store: &dyn TopicStore,
    topic: &Topic,
) -> Result<Vec<TopicMention>, ApiError> {
    let listed: Vec<Topic> = store
        .list()
        .await
        .map_err(map::error)?
        .into_iter()
        .map(|listing| listing.topic)
        .collect();
    let tree = TopicTree::of(&listed);
    let within: Vec<TopicId> = listed
        .iter()
        .map(|known| known.id)
        .filter(|id| tree.within(*id, topic.id))
        .collect();
    Ok(gateway
        .filing()?
        .mentions(&within, MENTIONS_SHOWN)
        .await
        .map_err(map::error)?
        .into_iter()
        .map(|mention| TopicMention {
            topic_id: mention.entity.to_string(),
            name: mention.name.as_str().to_owned(),
        })
        .collect())
}

fn refused(error: TopicPageError) -> ApiError {
    match error {
        TopicPageError::Wiki(WikiStoreError::DirectoryMissing) => {
            ApiError::Unavailable("wiki directory is not configured".into())
        }
        other @ (TopicPageError::Topic(_)
        | TopicPageError::Wiki(_)
        | TopicPageError::Memory(_)
        | TopicPageError::ParentPlacement(_)
        | TopicPageError::Payload(_)) => map::page_error(other),
    }
}

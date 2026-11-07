mod decide;
mod facts;
mod filing;
mod proposals;

use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing;
use chrono::DateTime;
use chrono::Utc;
use kernel::review::Marks;
use kernel::review::ReviewError;
use kernel::review::ReviewKey;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicTree;
use uuid::Uuid;
use wire::gateway::review::ReviewDecision;
use wire::gateway::review::ReviewItem;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/review",
            routing::get(queue).fallback(super::method_not_allowed),
        )
        .route(
            "/api/review/{item}/decision",
            routing::post(decision).fallback(super::method_not_allowed),
        )
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

struct Item {
    key: ReviewKey,
    body: ReviewItem,
    action: Action,
}

enum Action {
    Fact(facts::FactAction),
    Filing(filing::FilingAction),
    Tag(proposals::TagAction),
    Tidy(kernel::tidy::ProposalId),
}

#[derive(Clone, Copy)]
enum Source {
    Facts,
    Filing,
    Tags,
    Tidy,
}

impl Source {
    const ALL: [Self; 4] = [Self::Facts, Self::Filing, Self::Tags, Self::Tidy];

    const fn of(key: &ReviewKey) -> Self {
        match key {
            ReviewKey::Conflict { .. } | ReviewKey::Fact { .. } => Self::Facts,
            ReviewKey::Filing { .. } => Self::Filing,
            ReviewKey::Tag(_) => Self::Tags,
            ReviewKey::Tidy(_) => Self::Tidy,
        }
    }

    async fn items(
        self,
        gateway: &Gateway,
        topics: &Topics,
        now: DateTime<Utc>,
    ) -> Result<Vec<Item>, ApiError> {
        match self {
            Self::Facts => facts::items(gateway, topics, now).await,
            Self::Filing => filing::items(gateway, topics).await,
            Self::Tags => proposals::tags(gateway).await,
            Self::Tidy => proposals::tidy(gateway, topics).await,
        }
    }
}

struct Topics {
    by_id: BTreeMap<TopicId, Topic>,
    tree: TopicTree,
}

impl Topics {
    async fn load(gateway: &Gateway) -> Result<Self, ApiError> {
        let topics: Vec<Topic> = gateway
            .topics()?
            .list()
            .await
            .map_err(super::topics::topic_error)?
            .into_iter()
            .map(|listing| listing.topic)
            .collect();
        let tree = TopicTree::of(&topics);
        Ok(Self {
            by_id: topics.into_iter().map(|topic| (topic.id, topic)).collect(),
            tree,
        })
    }

    fn get(&self, id: TopicId) -> Option<&Topic> {
        self.by_id.get(&id)
    }

    fn name(&self, id: TopicId) -> String {
        self.get(id)
            .map_or_else(|| id.to_string(), |topic| topic.name.as_str().to_owned())
    }

    fn path(&self, id: TopicId) -> Vec<String> {
        let mut path: Vec<String> = self
            .tree
            .ancestors(id)
            .map(|above| self.name(above))
            .collect();
        path.reverse();
        path.push(self.name(id));
        path
    }
}

fn session_of(key: &str) -> Option<SessionId> {
    if Uuid::parse_str(key).is_ok() {
        return Some(SessionId::new(HarnessId::ClaudeCode, key));
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a conversation key that names no harness session has no session to review it under"
        )
    )]
    let session = SessionId::from_str(key).ok();
    session
}

fn quote_highlight(quote: &str, value: &str) -> Option<String> {
    quote.contains(value).then(|| value.to_owned())
}

pub(super) fn review_error(error: ReviewError) -> ApiError {
    match error {
        ReviewError::Unavailable(source) => {
            tracing::error!("review store failed: {source}");
            ApiError::Unavailable("review store unavailable".into())
        }
    }
}

async fn queue(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<Vec<ReviewItem>>, ApiError> {
    Ok(Json(open(&gateway, &user, Utc::now()).await?))
}

pub(crate) async fn open(
    gateway: &Gateway,
    user: &UserId,
    now: DateTime<Utc>,
) -> Result<Vec<ReviewItem>, ApiError> {
    let marks = Marks::of(
        gateway
            .review()?
            .marks(user.owner())
            .await
            .map_err(review_error)?,
    );
    let topics = Topics::load(gateway).await?;
    let mut items = Vec::new();
    for source in Source::ALL {
        items.extend(source.items(gateway, &topics, now).await?);
    }
    let mut open: Vec<ReviewItem> = items
        .into_iter()
        .filter(|item| marks.open(&item.key, now))
        .map(|item| item.body)
        .collect();
    open.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(open)
}

async fn decision(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Body(decision): Body<ReviewDecision>,
) -> Result<StatusCode, ApiError> {
    let missing = || ApiError::Missing(format!("unknown review item {segment}"));
    let key = ReviewKey::from_str(&segment).map_err(|_| missing())?;
    let store = gateway.review()?;
    let marks = Marks::of(store.marks(user.owner()).await.map_err(review_error)?);
    if marks.decided(&key) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let now = Utc::now();
    let topics = Topics::load(&gateway).await?;
    let item = Source::of(&key)
        .items(&gateway, &topics, now)
        .await?
        .into_iter()
        .find(|item| item.key == key)
        .ok_or_else(missing)?;
    let mark = decide::apply(&gateway, &user, item, decision, now).await?;
    store
        .mark(user.owner(), &mark)
        .await
        .map_err(review_error)?;
    Ok(StatusCode::NO_CONTENT)
}

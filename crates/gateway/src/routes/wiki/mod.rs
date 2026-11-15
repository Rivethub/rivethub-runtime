mod html;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use axum::routing::get;
use kernel::wiki::WIKI_LIMIT_MAX;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiLookup;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::WikiSlugError;
use wire::gateway::GatewayError;
use wire::gateway::wiki::WikiGapsResponse;
use wire::gateway::wiki::WikiMissBody;
use wire::gateway::wiki::WikiMissSuggestion;
use wire::gateway::wiki::WikiPageQuery;
use wire::gateway::wiki::WikiQuery;
use wire::gateway::wiki::WikiTopicList;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Query;

#[derive(Debug, thiserror::Error)]
enum WikiError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("wiki contains an invalid slug")]
    StoredSlug(#[from] WikiSlugError),
    #[error("wiki link construction failed")]
    Link(#[from] url::ParseError),
    #[error("wiki rendering failed")]
    Render(#[from] std::fmt::Error),
}

impl IntoResponse for WikiError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::StoredSlug(_) | Self::Link(_) | Self::Render(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(GatewayError::new("wiki response failed")),
            )
                .into_response(),
        }
    }
}

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/api/wiki", get(topics).fallback(super::method_not_allowed))
        .route(
            "/api/wiki/topics",
            get(topics).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/search",
            get(topics).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/_gaps",
            get(gaps).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/page",
            get(page_query).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/{slug}",
            get(page).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/{slug}/raw",
            get(raw).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/{slug}/archive",
            get(archive_raw).fallback(super::method_not_allowed),
        )
        .route(
            "/api/wiki/{slug}/archive/{month}",
            get(archive_month_raw).fallback(super::method_not_allowed),
        )
        .route("/wiki", get(index).fallback(super::method_not_allowed))
        .route("/wiki/", get(index).fallback(super::method_not_allowed))
        .route(
            "/wiki/{slug}",
            get(article).fallback(super::method_not_allowed),
        )
        .route(
            "/wiki/{slug}/archive",
            get(archive_html).fallback(super::method_not_allowed),
        )
        .route(
            "/wiki/{slug}/archive/{month}",
            get(archive_month_html).fallback(super::method_not_allowed),
        )
}

async fn topics(
    State(gateway): State<Arc<Gateway>>,
    Query(query): Query<WikiQuery>,
) -> Result<Json<WikiTopicList>, ApiError> {
    Ok(Json(crate::memory_body::wiki_topic_list_body(
        gateway.store().wiki_topics(filter(query)).await?,
    )))
}

async fn gaps(
    State(gateway): State<Arc<Gateway>>,
    Query(query): Query<WikiQuery>,
) -> Result<Json<WikiGapsResponse>, ApiError> {
    Ok(Json(crate::memory_body::wiki_gaps_response_body(
        gateway.store().wiki_gaps(filter(query).limit).await?,
    )))
}

async fn page(
    State(gateway): State<Arc<Gateway>>,
    Path(slug): Path<String>,
) -> Result<Response, WikiError> {
    respond(&gateway, requested_slug(slug)?, Format::Json).await
}

async fn raw(
    State(gateway): State<Arc<Gateway>>,
    Path(slug): Path<String>,
) -> Result<Response, WikiError> {
    respond(&gateway, requested_slug(slug)?, Format::Raw).await
}

async fn archive_raw(
    State(gateway): State<Arc<Gateway>>,
    Path(slug): Path<String>,
) -> Result<Response, WikiError> {
    archive(&gateway, slug, None, Format::Raw).await
}

async fn archive_month_raw(
    State(gateway): State<Arc<Gateway>>,
    Path((slug, month)): Path<(String, String)>,
) -> Result<Response, WikiError> {
    archive(&gateway, slug, Some(month), Format::Raw).await
}

async fn archive_html(
    State(gateway): State<Arc<Gateway>>,
    Path(slug): Path<String>,
) -> Result<Response, WikiError> {
    archive(&gateway, slug, None, Format::Html).await
}

async fn archive_month_html(
    State(gateway): State<Arc<Gateway>>,
    Path((slug, month)): Path<(String, String)>,
) -> Result<Response, WikiError> {
    archive(&gateway, slug, Some(month), Format::Html).await
}

async fn archive(
    gateway: &Gateway,
    slug: String,
    month: Option<String>,
    format: Format,
) -> Result<Response, WikiError> {
    let slug = requested_slug(slug)?;
    let markdown = gateway
        .store()
        .wiki_archive(&slug, month.as_deref())
        .await
        .map_err(ApiError::from)?;
    Ok(match (markdown, format) {
        (Some(markdown), Format::Html) => {
            html::response(StatusCode::OK, html::archive(slug.as_ref(), &markdown)?)
        }
        (Some(markdown), Format::Raw | Format::Json) => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            markdown,
        )
            .into_response(),
        (None, Format::Html) => {
            html::response(StatusCode::NOT_FOUND, html::missing(&archive_missing())?)
        }
        (None, Format::Raw | Format::Json) => {
            (StatusCode::NOT_FOUND, Json(archive_missing())).into_response()
        }
    })
}

fn archive_missing() -> WikiMissBody {
    WikiMissBody {
        error: "wiki archive not found".to_owned(),
        suggestions: Vec::new(),
    }
}

async fn page_query(
    State(gateway): State<Arc<Gateway>>,
    Query(query): Query<WikiPageQuery>,
) -> Result<Response, WikiError> {
    respond(&gateway, requested_slug(query.slug.into())?, Format::Json).await
}

async fn index(
    State(gateway): State<Arc<Gateway>>,
    Query(query): Query<WikiQuery>,
) -> Result<Response, WikiError> {
    let filter = filter(query);
    let topics = crate::memory_body::wiki_topic_list_body(
        gateway
            .store()
            .wiki_topics(filter.clone())
            .await
            .map_err(ApiError::from)?,
    );
    Ok(html::response(
        StatusCode::OK,
        html::index(&topics, &filter)?,
    ))
}

async fn article(
    State(gateway): State<Arc<Gateway>>,
    Path(slug): Path<String>,
) -> Result<Response, WikiError> {
    respond(&gateway, requested_slug(slug)?, Format::Html).await
}

fn filter(query: WikiQuery) -> WikiFilter {
    #[expect(clippy::manual_unwrap_or_default)]
    let offset = match query.offset {
        Some(offset) => offset,
        None => 0,
    };
    WikiFilter {
        query: query.q.filter(|text| !text.trim().is_empty()),
        tag: query.tag,
        limit: match query.limit {
            Some(limit) => limit.clamp(1, WIKI_LIMIT_MAX),
            None => 50,
        },
        offset,
    }
}

fn requested_slug(slug: String) -> Result<WikiSlug, ApiError> {
    WikiSlug::try_from(slug).map_err(|error| ApiError::BadRequest(error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Raw,
    Html,
}

impl Format {
    fn location(self, slug: &WikiSlug) -> Result<String, WikiError> {
        match self {
            Self::Json => page_url("/api/wiki/", slug),
            Self::Raw => Ok(format!("{}/raw", page_url("/api/wiki/", slug)?)),
            Self::Html => page_url("/wiki/", slug),
        }
    }
}

async fn respond(gateway: &Gateway, slug: WikiSlug, format: Format) -> Result<Response, WikiError> {
    match gateway
        .store()
        .wiki_page(&slug)
        .await
        .map_err(ApiError::from)?
    {
        WikiLookup::Page(page) => match format {
            Format::Json => {
                Ok(Json(crate::memory_body::wiki_page_response_body(*page)).into_response())
            }
            Format::Raw => Ok((
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                page.markdown,
            )
                .into_response()),
            Format::Html => Ok(html::response(
                StatusCode::OK,
                html::article(&crate::memory_body::wiki_page_response_body(*page))?,
            )),
        },
        WikiLookup::Redirect(target) => {
            let target = WikiSlug::try_from(target)?;
            if target == slug {
                return Err(WikiError::StoredSlug(WikiSlugError));
            }
            Ok(Redirect::permanent(&format.location(&target)?).into_response())
        }
        WikiLookup::Missing => {
            let matches = gateway
                .store()
                .wiki_topics(WikiFilter {
                    query: Some(slug.as_ref().to_owned()),
                    tag: None,
                    limit: 5,
                    offset: 0,
                })
                .await
                .map_err(ApiError::from)?;
            let body = WikiMissBody {
                error: "wiki page not found".to_owned(),
                suggestions: matches
                    .topics
                    .into_iter()
                    .map(|topic| WikiMissSuggestion {
                        slug: topic.slug,
                        title: topic.title,
                    })
                    .collect(),
            };
            match format {
                Format::Html => Ok(html::response(StatusCode::NOT_FOUND, html::missing(&body)?)),
                Format::Json | Format::Raw => {
                    Ok((StatusCode::NOT_FOUND, Json(body)).into_response())
                }
            }
        }
    }
}

fn page_url(prefix: &str, slug: &WikiSlug) -> Result<String, WikiError> {
    let mut url = url::Url::parse("http://localhost")?;
    url.set_path(prefix);
    url.path_segments_mut()
        .map_err(|()| WikiError::StoredSlug(WikiSlugError))?
        .pop_if_empty()
        .push(slug.as_ref());
    Ok(url.path().to_owned())
}

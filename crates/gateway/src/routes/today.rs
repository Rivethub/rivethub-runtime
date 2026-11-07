use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing;
use chrono::DateTime;
use chrono::Datelike as _;
use chrono::Local;
use chrono::NaiveDate;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::changes::Change;
use kernel::changes::ChangeActor;
use kernel::changes::ChangeKind;
use kernel::changes::ChangeWindow;
use kernel::changes::DigestRequest;
use kernel::changes::Subject;
use kernel::changes::SubjectKey;
use kernel::memory::SESSION_PAGE_MAX;
use kernel::memory::SessionCursor;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use kernel::memory::SessionView;
use kernel::memory_core::day_window;
use kernel::topics::TopicId;
use serde::Deserialize;
use wire::gateway::entities;
use wire::gateway::review;
use wire::gateway::review::ReviewGroup;
use wire::gateway::review::ReviewItem;
use wire::gateway::session_catalog::SharedWith;
use wire::gateway::today;
use wire::session_id::SessionId;

use super::session_delete::catalog::Lineage;
use super::session_delete::catalog::{catalog, lineage, row, shown, visibilities};
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Query;
use crate::tenancy::UserId;

const CHANGES_DEFAULT: usize = 200;
const CHANGES_MAX: usize = 1000;
const LOOKBACK_DAYS: i64 = 30;
const DAY_PAGES: usize = 5;
const NEEDS_YOU: usize = 3;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/today",
            routing::get(digest).fallback(super::method_not_allowed),
        )
        .route(
            "/api/changes",
            routing::get(changes).fallback(super::method_not_allowed),
        )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TodayParams {
    date: Option<NaiveDate>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChangeParams {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    subject: Option<String>,
    limit: Option<usize>,
}

impl From<kernel::changes::ChangeError> for ApiError {
    fn from(error: kernel::changes::ChangeError) -> Self {
        Self::Unavailable(error.to_string())
    }
}

fn subject_key(text: &str) -> Result<SubjectKey, ApiError> {
    TopicId::from_str(text)
        .map(SubjectKey::Topic)
        .or_else(|_| SessionId::from_str(text).map(SubjectKey::Session))
        .map_err(|_| {
            ApiError::BadRequest("subject is not a topic, entity or session id".to_owned())
        })
}

fn bounds(date: NaiveDate) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    let range = day_window(date, &Local);
    range
        .since
        .zip(range.before)
        .ok_or_else(|| ApiError::BadRequest("date is out of range".to_owned()))
}

fn month_end(date: NaiveDate) -> Result<DateTime<Utc>, ApiError> {
    let first = date
        .with_day(1)
        .and_then(|first| first.checked_add_months(chrono::Months::new(1)))
        .ok_or_else(|| ApiError::BadRequest("date is out of range".to_owned()))?;
    bounds(first).map(|(start, _)| start)
}

fn month_start(date: NaiveDate) -> Result<DateTime<Utc>, ApiError> {
    let first = date
        .with_day(1)
        .ok_or_else(|| ApiError::BadRequest("date is out of range".to_owned()))?;
    bounds(first).map(|(start, _)| start)
}

struct Titles(BTreeMap<SessionId, String>);

impl Titles {
    async fn load(
        gateway: &Gateway,
        known: &[SessionRecord],
        changes: &[Change],
    ) -> Result<Self, ApiError> {
        let mut titles: BTreeMap<SessionId, String> = known
            .iter()
            .map(|record| (record.id.clone(), record.title.clone()))
            .collect();
        let mentioned = changes.iter().flat_map(|change| {
            let subject = match &change.subject {
                Subject::Session(session) => Some(session.clone()),
                Subject::Topic { .. } | Subject::Entity { .. } => None,
            };
            subject.into_iter().chain(change.source.clone())
        });
        if let Some(catalog) = gateway.session_catalog() {
            for session in mentioned {
                if titles.contains_key(&session) {
                    continue;
                }
                if let Some(record) = catalog.get(&session).await? {
                    titles.insert(session, record.title);
                }
            }
        }
        Ok(Self(titles))
    }

    fn of(&self, session: &SessionId) -> String {
        self.0
            .get(session)
            .cloned()
            .unwrap_or_else(|| session.native().to_owned())
    }

    fn subject(&self, subject: &Subject) -> review::SubjectRef {
        match subject {
            Subject::Topic { id, title } => review::SubjectRef::Topic {
                id: id.to_string(),
                title: title.clone(),
            },
            Subject::Entity { id, title } => review::SubjectRef::Entity {
                id: id.to_string(),
                title: title.clone(),
            },
            Subject::Session(session) => review::SubjectRef::Session {
                id: session.clone(),
                title: self.of(session),
            },
        }
    }

    fn change(&self, change: &Change) -> today::Change {
        today::Change {
            kind: kind(change.kind),
            subject: self.subject(&change.subject),
            text: change.text.clone(),
            at: change.at,
            actor: match change.actor {
                ChangeActor::You => today::Actor::You,
                ChangeActor::WikiWriter => today::Actor::WikiWriter,
                ChangeActor::Categorizer => today::Actor::Categorizer,
            },
            source: change.source.as_ref().map(|session| today::SessionRef {
                kind: today::SessionKind::Session,
                id: session.clone(),
                title: self.of(session),
            }),
        }
    }
}

pub(crate) const fn kind(kind: ChangeKind) -> entities::ChangeKind {
    match kind {
        ChangeKind::Fact => entities::ChangeKind::Fact,
        ChangeKind::Decision => entities::ChangeKind::Decision,
        ChangeKind::New => entities::ChangeKind::New,
        ChangeKind::Filed => entities::ChangeKind::Filed,
        ChangeKind::Conflict => entities::ChangeKind::Conflict,
        ChangeKind::Tag => entities::ChangeKind::Tag,
        ChangeKind::Merge => entities::ChangeKind::Merge,
    }
}

async fn changes(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<ChangeParams>,
) -> Result<Json<Vec<today::Change>>, ApiError> {
    let window = ChangeWindow {
        from: params.from,
        to: params.to.unwrap_or_else(Utc::now),
        subject: params.subject.as_deref().map(subject_key).transpose()?,
    };
    let mut found = gateway.changes()?.changes(&window).await?;
    found.truncate(
        params
            .limit
            .unwrap_or(CHANGES_DEFAULT)
            .clamp(1, CHANGES_MAX),
    );
    let titles = Titles::load(&gateway, &[], &found).await?;
    Ok(Json(
        found.iter().map(|change| titles.change(change)).collect(),
    ))
}

async fn day_sessions(
    gateway: &Gateway,
    user: &UserId,
    lineage: &Lineage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<SessionRecord>, ApiError> {
    let catalog = catalog(gateway)?;
    let mut query = SessionQuery::new(SessionView::All);
    query.limit = SESSION_PAGE_MAX;
    query.exclude = lineage.parents.keys().cloned().collect();
    query.after = Some(SessionCursor {
        started_at: end,
        key: String::new(),
    });
    let mut found = Vec::new();
    for _ in 0..DAY_PAGES {
        let page = catalog.list(&query).await?;
        let reached_start = page.items.iter().any(|record| record.started_at < start);
        found.extend(
            page.items
                .into_iter()
                .filter(|record| (start..end).contains(&record.started_at))
                .filter(|record| shown(gateway, user, record)),
        );
        match page.next {
            Some(next) if !reached_start => query.after = Some(next),
            Some(_) | None => break,
        }
    }
    Ok(found)
}

async fn sources(
    gateway: &Gateway,
    user: &UserId,
    have: &[SessionRecord],
    sessions: impl Iterator<Item = SessionId>,
) -> Result<Vec<SessionRecord>, ApiError> {
    let catalog = catalog(gateway)?;
    let mut found: Vec<SessionRecord> = Vec::new();
    for session in sessions {
        if have.iter().chain(&found).any(|record| record.id == session) {
            continue;
        }
        if let Some(record) = catalog.get(&session).await?
            && !record.trashed
            && shown(gateway, user, &record)
        {
            found.push(record);
        }
    }
    Ok(found)
}

async fn summary(
    gateway: &Gateway,
    user: &UserId,
    date: NaiveDate,
    (start, end): (DateTime<Utc>, DateTime<Utc>),
    day: &[Change],
    sessions: usize,
) -> Result<String, ApiError> {
    let digests = gateway.digests()?;
    let kept = digests.digest(date, user.owner()).await?;
    let stale = kept.as_ref().is_none_or(|digest| {
        !usize::try_from(digest.changes).is_ok_and(|counted| counted == day.len())
    });
    if stale && !day.is_empty() {
        digests
            .request_digest(&DigestRequest::new(date, start, end, user.owner()))
            .await?;
    }
    Ok(match kept {
        Some(digest) => digest.summary,
        None => kernel::changes::fallback_summary(day, sessions),
    })
}

struct DaySessions {
    records: Vec<SessionRecord>,
    marks: BTreeMap<SessionId, SharedWith>,
    lineage: Lineage,
}

impl DaySessions {
    async fn load(
        gateway: &Gateway,
        user: &UserId,
        (start, end): (DateTime<Utc>, DateTime<Utc>),
        day: &[Change],
    ) -> Result<Self, ApiError> {
        let lineage = lineage(gateway).await;
        let mut records = day_sessions(gateway, user, &lineage, start, end).await?;
        let caused = day
            .iter()
            .filter_map(|change| change.source.as_ref())
            .map(|session| top(&lineage, session));
        let more = sources(gateway, user, &records, caused).await?;
        records.extend(more);
        records.sort_by(|left, right| {
            let last = |record: &SessionRecord| record.ended_at.unwrap_or(record.started_at);
            last(right).cmp(&last(left)).then(right.id.cmp(&left.id))
        });
        let ids: Vec<SessionId> = records.iter().map(|record| record.id.clone()).collect();
        let marks = visibilities(gateway, user, &ids).await?;
        Ok(Self {
            records,
            marks,
            lineage,
        })
    }

    fn entries(self, day: &[Change], titles: &Titles) -> Vec<today::TodayEntry> {
        let Self {
            records,
            marks,
            lineage,
        } = self;
        records
            .into_iter()
            .map(|record| {
                let changes = day
                    .iter()
                    .filter(|change| {
                        change
                            .source
                            .as_ref()
                            .is_some_and(|source| top(&lineage, source) == record.id)
                    })
                    .map(|change| titles.change(change))
                    .collect();
                let visibility = marks
                    .get(&record.id)
                    .cloned()
                    .unwrap_or(SharedWith::Private);
                today::TodayEntry {
                    session: row(record, visibility, &lineage),
                    changes,
                }
            })
            .collect()
    }
}

fn top(lineage: &Lineage, session: &SessionId) -> SessionId {
    lineage
        .parents
        .get(&session.to_string())
        .cloned()
        .unwrap_or_else(|| session.clone())
}

async fn digest(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Query(params): Query<TodayParams>,
) -> Result<Json<today::TodayDigest>, ApiError> {
    let today = Utc::now().with_timezone(&Local).date_naive();
    let date = params.date.unwrap_or(today);
    let (start, end) = bounds(date)?;
    let from = month_start(date)?.min(start - TimeDelta::days(LOOKBACK_DAYS));
    let all = gateway
        .changes()?
        .changes(&ChangeWindow::between(from, month_end(date)?.max(end)))
        .await?;
    let before_end: Vec<Change> = all
        .iter()
        .filter(|change| change.at < end)
        .cloned()
        .collect();
    let day: Vec<Change> = before_end
        .iter()
        .filter(|change| change.at >= start)
        .cloned()
        .collect();
    let sessions = DaySessions::load(&gateway, &user, (start, end), &day).await?;
    let titles = Titles::load(&gateway, &sessions.records, &all).await?;
    let summary = summary(
        &gateway,
        &user,
        date,
        (start, end),
        &day,
        sessions.records.len(),
    )
    .await?;
    let entries = sessions.entries(&day, &titles);
    let queue = if gateway.review.is_some() {
        super::review::open(&gateway, &user, Utc::now()).await?
    } else {
        Vec::new()
    };
    let review_count = u32::try_from(queue.len()).map_err(|_| ApiError::Internal)?;
    Ok(Json(today::TodayDigest {
        date,
        headline: kernel::changes::headline(kernel::changes::learned(&day), date, today),
        summary,
        needs_you: needs_you(queue),
        review_count,
        entries,
        activity_by_day: kernel::changes::activity(&all, date, &Local)
            .into_iter()
            .map(|entry| today::ActivityDay {
                date: entry.date,
                level: entry.level,
            })
            .collect(),
        recently_updated: kernel::changes::recently_updated(&before_end)
            .iter()
            .map(|subject| titles.subject(subject))
            .collect(),
        freshness: entities::Freshness::Live,
    }))
}

fn needs_you(mut queue: Vec<ReviewItem>) -> Vec<ReviewItem> {
    queue.sort_by_key(|item| item.group != ReviewGroup::Conflict);
    queue.truncate(NEEDS_YOU);
    queue
}

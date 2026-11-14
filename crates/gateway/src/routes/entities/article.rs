use std::collections::BTreeMap;
use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::EntityHistory;
use kernel::facts::FactSession;
use kernel::facts::FactState;
use kernel::facts::SessionLink;
use kernel::facts::connected;
use kernel::facts::facts_as_of;
use kernel::facts::timeline;
use kernel::tenancy::SessionVisibility;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use kernel::topics::Topic;
use kernel::topics::bound_page;
use wire::gateway::entities::ChangeKind;
use wire::gateway::entities::ConnectedEntity;
use wire::gateway::entities::EntityArticle;
use wire::gateway::entities::EntitySummary;
use wire::gateway::entities::Freshness;
use wire::gateway::entities::TimelineEntry;
use wire::gateway::session_catalog::SessionMemoryState;
use wire::gateway::session_catalog::SessionSummaryRow;
use wire::session_id::SessionId;

use super::Filed;
use super::Known;
use super::map;
use super::map::Titles;
use crate::Gateway;
use crate::error::ApiError;

const CONNECTED_SHOWN: usize = 24;

pub(super) async fn build(
    gateway: &Gateway,
    known: &Known,
    entity: &Topic,
    kind: &Filed,
    viewer: &Viewer,
    as_of: Option<DateTime<Utc>>,
) -> Result<EntityArticle, ApiError> {
    let facts = gateway.facts()?;
    let claims: Vec<Claim> = facts
        .claims(entity.id)
        .await
        .map_err(map::error)?
        .into_iter()
        .filter(|claim| as_of.is_none_or(|cut| claim.at <= cut))
        .collect();
    let written: Vec<EntityHistory> = facts
        .entity_history(entity.id)
        .await
        .map_err(map::error)?
        .into_iter()
        .filter(|line| as_of.is_none_or(|cut| line.at <= cut))
        .collect();
    let own = sessions_of(&claims);
    let mut named = own.clone();
    for line in &written {
        if let Some(session) = &line.session
            && !named.contains(session)
        {
            named.push(session.clone());
        }
    }
    let resolved = facts.fact_sessions(&named).await.map_err(map::error)?;
    let titles = Titles::of(&resolved);
    let found: Vec<FactSession> = resolved
        .into_iter()
        .filter(|session| own.iter().any(|id| id == &session.session))
        .collect();
    let links: Vec<SessionLink> = facts
        .session_links()
        .await
        .map_err(map::error)?
        .into_iter()
        .filter(|link| link.entity != entity.id)
        .chain(own.iter().map(|session| SessionLink {
            entity: entity.id,
            session: session.clone(),
        }))
        .collect();
    let sessions = rows(gateway, viewer, found).await?;
    Ok(EntityArticle {
        summary: EntitySummary {
            id: entity.id.to_string(),
            title: entity.name.as_str().to_owned(),
            entity_type: kind.key.clone(),
            type_name: kind.name.clone(),
            session_count: own.len(),
        },
        aliases: entity
            .aliases
            .iter()
            .map(|alias| alias.as_str().to_owned())
            .collect(),
        topic: None,
        visibility: crate::tenancy::shared_with(&entity.visibility),
        lead: lead(gateway, entity).await?,
        facts: facts_as_of(&claims, None)
            .iter()
            .map(|state| map::fact(state, &titles))
            .collect(),
        timeline: history(entity, &claims, &written, as_of, &titles),
        connected: connected(entity.id, &links)
            .into_iter()
            .take(CONNECTED_SHOWN)
            .filter_map(|(id, shared)| {
                known.title(id).map(|title| ConnectedEntity {
                    id: id.to_string(),
                    title: title.to_owned(),
                    shared_sessions: shared,
                })
            })
            .collect(),
        sessions,
        tags: Vec::new(),
        freshness: Freshness::Live,
    })
}

pub(super) async fn titles(gateway: &Gateway, states: &[FactState]) -> Result<Titles, ApiError> {
    let sessions: BTreeSet<SessionId> = states
        .iter()
        .flat_map(|state| state.sources.iter().chain(&state.candidates))
        .map(|support| support.session.clone())
        .collect();
    let sessions: Vec<SessionId> = sessions.into_iter().collect();
    let found = gateway
        .facts()?
        .fact_sessions(&sessions)
        .await
        .map_err(map::error)?;
    Ok(Titles::of(&found))
}

fn sessions_of(claims: &[Claim]) -> Vec<SessionId> {
    let sessions: BTreeSet<SessionId> = claims
        .iter()
        .filter_map(|claim| claim.said.session().cloned())
        .collect();
    sessions.into_iter().collect()
}

fn history(
    entity: &Topic,
    claims: &[Claim],
    lines: &[EntityHistory],
    as_of: Option<DateTime<Utc>>,
    titles: &Titles,
) -> Vec<TimelineEntry> {
    let mut entries: Vec<TimelineEntry> = timeline(claims, None)
        .iter()
        .map(|event| map::event(event, titles))
        .collect();
    for line in lines {
        entries.push(TimelineEntry {
            at: line.at,
            text: line.sentence.clone(),
            kind: ChangeKind::History,
            source: line
                .session
                .as_ref()
                .map(|session| map::mention(session, titles)),
        });
    }
    if as_of.is_none_or(|cut| entity.created_at <= cut) {
        entries.push(TimelineEntry {
            at: entity.created_at,
            text: format!("{} was added", entity.name.as_str()),
            kind: ChangeKind::New,
            source: None,
        });
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.at));
    entries
}

async fn lead(gateway: &Gateway, entity: &Topic) -> Result<String, ApiError> {
    let page = bound_page(gateway.topics()?.as_ref(), gateway.store().as_ref(), entity)
        .await
        .map_err(super::super::topics::page_error)?;
    Ok(match page {
        Some(page) => page.current_state,
        None => String::new(),
    })
}

fn effective(found: Option<&SessionVisibility>) -> &Visibility {
    match found {
        Some(state) => &state.effective,
        None => &Visibility::Private,
    }
}

async fn rows(
    gateway: &Gateway,
    viewer: &Viewer,
    found: Vec<FactSession>,
) -> Result<Vec<SessionSummaryRow>, ApiError> {
    let keys: Vec<SessionId> = found
        .iter()
        .map(|session| session.session.clone())
        .collect();
    let visibility = gateway
        .tenancy_store()?
        .session_visibility(viewer, &keys)
        .await
        .map_err(super::super::topics::visibility_error)?;
    let kept: BTreeMap<String, DateTime<Utc>> = match gateway.session_deletion() {
        Some(deletions) => deletions
            .kept(
                &keys
                    .iter()
                    .map(|key| key.as_str().to_owned())
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(ApiError::Store)?,
        None => BTreeMap::new(),
    };
    let mut rows: Vec<SessionSummaryRow> = found
        .into_iter()
        .map(|session| SessionSummaryRow {
            title: session
                .title
                .unwrap_or_else(|| session.session.as_str().to_owned()),
            harness: Some(session.session.harness()),
            origin: None,
            started_at: session.started_at,
            ended_at: None,
            message_count: session.messages,
            topic: None,
            parent_id: None,
            child_count: None,
            visibility: crate::tenancy::shared_with(effective(visibility.get(&session.session))),
            kept: kept.contains_key(session.session.as_str()),
            trashed: false,
            memory_state: SessionMemoryState::Summarized,
            gist: None,
            id: session.session.into(),
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.started_at));
    Ok(rows)
}

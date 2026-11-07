use std::collections::BTreeMap;

use kernel::cleanup::EntityType as SeededType;
use kernel::facts::FactChange;
use kernel::facts::FactError;
use kernel::facts::FactEvent;
use kernel::facts::FactState;
use kernel::facts::Support;
use kernel::tenancy::Visibility;
use strum::IntoEnumIterator as _;
use wire::gateway::entities::EntityType;
use wire::gateway::entities::Fact;
use wire::gateway::entities::FactConflict;
use wire::gateway::entities::FactPrevious;
use wire::gateway::entities::FactSource;
use wire::gateway::entities::SessionMention;
use wire::gateway::entities::SharedWith;
use wire::gateway::entities::TimelineEntry;
use wire::session_id::SessionId;

use crate::error::ApiError;

pub(super) type Titles = BTreeMap<SessionId, String>;

pub(super) fn entity_type(root: &str) -> Option<EntityType> {
    let seeded = SeededType::iter().find(|seeded| seeded.to_string() == root)?;
    Some(match seeded {
        SeededType::People => EntityType::Person,
        SeededType::Agents => EntityType::Agent,
        SeededType::Hosts => EntityType::Machine,
        SeededType::Hardware => EntityType::Hardware,
        SeededType::Models => EntityType::Model,
        SeededType::Services => EntityType::Service,
        SeededType::Software
        | SeededType::ClisAndTools
        | SeededType::Apps
        | SeededType::LibrariesAndFrameworks
        | SeededType::OsAndPlatforms
        | SeededType::PluginsAndSkills => EntityType::Software,
        SeededType::Repos => EntityType::Repository,
        SeededType::Domains => EntityType::Domain,
        SeededType::Databases => EntityType::Database,
    })
}

pub(super) fn error(error: FactError) -> ApiError {
    match error {
        FactError::EmptyKey | FactError::EmptyValue | FactError::TooLong { .. } => {
            ApiError::BadRequest(error.to_string())
        }
        FactError::Unavailable(source) => {
            tracing::error!("fact store failed: {source}");
            ApiError::Unavailable("fact store unavailable".into())
        }
    }
}

pub(super) fn shared_with(visibility: &Visibility) -> SharedWith {
    match visibility {
        Visibility::Private => SharedWith::Private,
        Visibility::Space(space) => SharedWith::Space {
            space: space.as_str().to_owned(),
        },
    }
}

pub(super) fn mention(session: &SessionId, titles: &Titles) -> SessionMention {
    SessionMention {
        id: session.clone(),
        title: titles
            .get(session)
            .cloned()
            .unwrap_or_else(|| session.as_str().to_owned()),
    }
}

fn source(support: &Support, titles: &Titles) -> FactSource {
    FactSource {
        session: mention(&support.session, titles),
        at: support.at,
        value: support.value.as_str().to_owned(),
    }
}

pub(super) fn fact(state: &FactState, titles: &Titles) -> Fact {
    Fact {
        id: state.id.to_string(),
        key: state.key.as_str().to_owned(),
        value: state.value.as_str().to_owned(),
        since: state.since,
        pinned: state.pinned,
        sources: state
            .sources
            .iter()
            .map(|support| source(support, titles))
            .collect(),
        conflict: (!state.candidates.is_empty()).then(|| FactConflict {
            candidates: state
                .candidates
                .iter()
                .map(|support| source(support, titles))
                .collect(),
        }),
        previous: state.previous.as_ref().map(|previous| FactPrevious {
            value: previous.value.as_str().to_owned(),
            until: previous.until,
        }),
    }
}

pub(super) fn event(event: &FactEvent, titles: &Titles) -> TimelineEntry {
    let (kind, text) = kernel::changes::fact_text(event);
    let session = match &event.change {
        FactChange::Became { session, .. } | FactChange::Disputed { session, .. } => {
            session.as_ref()
        }
        FactChange::Pinned { .. } => None,
    };
    let kind = crate::routes::today::kind(kind);
    TimelineEntry {
        at: event.at,
        text,
        kind,
        source: session.map(|session| mention(session, titles)),
    }
}

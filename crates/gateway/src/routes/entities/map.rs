use std::collections::BTreeMap;

use kernel::facts::FactChange;
use kernel::facts::FactError;
use kernel::facts::FactEvent;
use kernel::facts::FactSession;
use kernel::facts::FactState;
use kernel::facts::Support;
use kernel::memory::Provenance;
use wire::gateway::entities::Fact;
use wire::gateway::entities::FactConflict;
use wire::gateway::entities::FactPrevious;
use wire::gateway::entities::FactSource;
use wire::gateway::entities::SessionMention;
use wire::gateway::entities::TimelineEntry;
use wire::session_id::SessionId;

use crate::error::ApiError;

#[derive(Default)]
pub(crate) struct Titles {
    names: BTreeMap<SessionId, String>,
    imported: BTreeMap<SessionId, Provenance>,
}

impl Titles {
    pub(crate) fn of(found: &[FactSession]) -> Self {
        let mut titles = Self::default();
        for session in found {
            if let Some(title) = &session.title {
                titles.names.insert(session.session.clone(), title.clone());
            }
            if let Some(provenance) = &session.provenance {
                titles
                    .imported
                    .insert(session.session.clone(), provenance.clone());
            }
        }
        titles
    }

    fn label(&self, session: &SessionId) -> String {
        let title = self
            .names
            .get(session)
            .map_or_else(|| session.as_str().to_owned(), Clone::clone);
        match self.imported.get(session) {
            Some(provenance) => format!(
                "{title} (imported from {}, {})",
                provenance.author_label(),
                provenance.verification.short()
            ),
            None => title,
        }
    }
}

pub(crate) fn error(error: FactError) -> ApiError {
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

pub(crate) fn mention(session: &SessionId, titles: &Titles) -> SessionMention {
    SessionMention {
        id: session.clone(),
        title: titles.label(session),
    }
}

pub(crate) fn source(support: &Support, titles: &Titles) -> FactSource {
    FactSource {
        session: mention(&support.session, titles),
        at: support.at,
        value: support.value.as_str().to_owned(),
        imported_from: titles
            .imported
            .get(&support.session)
            .map(crate::memory_body::provenance_body),
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

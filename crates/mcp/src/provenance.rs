use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore;
use kernel::memory::Provenance;

use crate::Error;
use crate::render_memory;

const LABEL_CHARS: usize = 64;

pub(crate) type Origins = BTreeMap<String, Provenance>;

pub(crate) async fn resolve<'a>(
    store: &dyn MemoryStore,
    entries: impl IntoIterator<Item = &'a MemoryEntry>,
    more_keys: impl IntoIterator<Item = &'a str>,
) -> Result<Origins, Error> {
    Ok(kernel::memory::resolve_origins(store, entries, more_keys).await?)
}

pub(crate) fn tag(provenance: &Provenance, zone: chrono_tz::Tz, dated: bool) -> String {
    if provenance.team_owner.is_some() {
        return provenance.tag();
    }
    let date = provenance
        .origin_date
        .filter(|_| dated)
        .map(|date: DateTime<Utc>| render_memory::local_date(date, zone));
    match (&provenance.author, date) {
        (Some(author), Some(date)) => format!(
            "from {author} · {date} · {}",
            provenance.verification.short()
        ),
        _ => provenance.tag(),
    }
}

pub(crate) fn quoted(origins: &Origins, session_key: &str, text: String) -> String {
    match origins.get(session_key) {
        Some(provenance) => kernel::memory::quote(provenance, &text),
        None => text,
    }
}

pub(crate) fn own_text(imported: bool, text: &str) -> std::borrow::Cow<'_, str> {
    if imported {
        std::borrow::Cow::Owned(kernel::memory::label(text, LABEL_CHARS))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

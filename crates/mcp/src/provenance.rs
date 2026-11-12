use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore;
use kernel::memory::Provenance;

use crate::Error;
use crate::render_memory;

pub(crate) type Origins = BTreeMap<String, Provenance>;

pub(crate) async fn lookup<'a>(
    store: &dyn MemoryStore,
    session_keys: impl IntoIterator<Item = &'a str>,
) -> Result<Origins, Error> {
    Ok(kernel::memory::origins_of(store, session_keys).await?)
}

pub(crate) fn tag(provenance: &Provenance, zone: chrono_tz::Tz, dated: bool) -> String {
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

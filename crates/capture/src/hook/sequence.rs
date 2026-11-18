use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Write as _};
use std::num::NonZeroU64;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Position {
    hook_producer: Uuid,
    hook_sequence: NonZeroU64,
}

#[derive(Serialize, Deserialize)]
struct Ledger {
    producer: Uuid,
    events: BTreeMap<String, NonZeroU64>,
}

pub(super) fn position(path: &Path, event_id: String) -> io::Result<Position> {
    let mut ledger: Ledger = match File::open(path) {
        Ok(file) => serde_json::from_reader(BufReader::new(file))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ledger {
            producer: Uuid::new_v4(),
            events: BTreeMap::new(),
        },
        Err(error) => return Err(error),
    };
    let hook_sequence = if let Some(sequence) = ledger.events.get(&event_id) {
        *sequence
    } else {
        let size = u64::try_from(ledger.events.len()).map_err(io::Error::other)?;
        let sequence = size
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or_else(|| io::Error::other("hook sequence is full"))?;
        ledger.events.insert(event_id, sequence);
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("hook sequence has no state directory"))?;
        let mut staged = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(staged.as_file_mut(), &ledger)?;
        staged.flush()?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        File::open(dir)?.sync_all()?;
        sequence
    };
    Ok(Position {
        hook_producer: ledger.producer,
        hook_sequence,
    })
}

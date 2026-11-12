use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use capture::bundle::Sealed;
use capture::sanitize::LocalReview;
use capture::sanitize::Report;
use capture::sanitize::decide::Resolution;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::share::desk::DRAFT_IDLE;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::Draft;
use kernel::memory::share::desk::DraftId;
use kernel::memory::share::desk::MAX_DRAFT_BYTES;
use kernel::memory::share::desk::MAX_DRAFTS;
use tokio::sync::Mutex;
use uuid::Uuid;

pub(super) struct Drafts {
    slots: parking_lot::Mutex<BTreeMap<DraftId, Slot>>,
    max_bytes: usize,
}

struct Slot {
    bytes: usize,
    touched: DateTime<Utc>,
    draft: Held,
}

enum Held {
    Reserved,
    Ready(Arc<Mutex<DraftState>>),
}

pub(super) struct DraftState {
    pub(super) phase: Phase,
    pub(super) sealed: Sealed,
    pub(super) local: Option<LocalReview>,
    pub(super) report: Report,
    pub(super) resolved: BTreeMap<usize, Resolution>,
    pub(super) conversations: Vec<Uuid>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Open,
    Sending,
    Closed,
}

impl DraftState {
    pub(super) fn open(&self) -> Result<(), DeskError> {
        match self.phase {
            Phase::Open => Ok(()),
            Phase::Sending | Phase::Closed => Err(DeskError::UnknownDraft),
        }
    }
}

pub(super) struct Reservation<'drafts> {
    drafts: &'drafts Drafts,
    id: DraftId,
    kept: bool,
}

impl Reservation<'_> {
    pub(super) fn commit(
        mut self,
        state: DraftState,
        now: DateTime<Utc>,
    ) -> Result<Draft, DeskError> {
        let held = held_bytes(&state.sealed, state.local.as_ref(), &state.report)?;
        let expires_at = self.drafts.resize(self.id, held, now)?;
        let draft = super::view(self.id, &state, expires_at);
        if let Some(slot) = self.drafts.slots.lock().get_mut(&self.id) {
            slot.draft = Held::Ready(Arc::new(Mutex::new(state)));
        }
        self.kept = true;
        Ok(draft)
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.kept {
            self.drafts.slots.lock().remove(&self.id);
        }
    }
}

impl Default for Drafts {
    fn default() -> Self {
        Self {
            slots: parking_lot::Mutex::default(),
            max_bytes: MAX_DRAFT_BYTES,
        }
    }
}

impl Drafts {
    pub(super) const fn with_byte_cap(mut self, bytes: usize) -> Self {
        self.max_bytes = bytes;
        self
    }

    pub(super) fn len(&self) -> usize {
        self.slots.lock().len()
    }

    pub(super) fn sweep(&self, now: DateTime<Utc>) {
        self.slots.lock().retain(|_, slot| {
            matches!(slot.draft, Held::Reserved) || slot.touched + DRAFT_IDLE > now
        });
    }

    pub(super) fn get(
        &self,
        id: DraftId,
        now: DateTime<Utc>,
    ) -> Result<(Arc<Mutex<DraftState>>, DateTime<Utc>), DeskError> {
        match self.slots.lock().get_mut(&id) {
            Some(Slot {
                touched,
                draft: Held::Ready(state),
                ..
            }) => {
                *touched = now;
                Ok((Arc::clone(state), now + DRAFT_IDLE))
            }
            Some(Slot {
                draft: Held::Reserved,
                ..
            })
            | None => Err(DeskError::UnknownDraft),
        }
    }

    pub(super) fn remove(&self, id: DraftId) {
        self.slots.lock().remove(&id);
    }

    pub(super) fn reserve(
        &self,
        estimate: usize,
        now: DateTime<Utc>,
    ) -> Result<Reservation<'_>, DeskError> {
        let mut slots = self.slots.lock();
        let bytes: usize = slots.values().map(|slot| slot.bytes).sum();
        if slots.len() >= MAX_DRAFTS || bytes + estimate > self.max_bytes {
            return Err(DeskError::Full);
        }
        let id = DraftId::random();
        slots.insert(
            id,
            Slot {
                bytes: estimate,
                touched: now,
                draft: Held::Reserved,
            },
        );
        drop(slots);
        Ok(Reservation {
            drafts: self,
            id,
            kept: false,
        })
    }

    pub(super) fn resize(
        &self,
        id: DraftId,
        bytes: usize,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, DeskError> {
        let mut slots = self.slots.lock();
        let others: usize = slots
            .iter()
            .filter(|(other, _)| **other != id)
            .map(|(_, slot)| slot.bytes)
            .sum();
        if others + bytes > self.max_bytes {
            return Err(DeskError::Full);
        }
        let slot = slots.get_mut(&id).ok_or(DeskError::UnknownDraft)?;
        slot.bytes = bytes;
        slot.touched = now;
        drop(slots);
        Ok(now + DRAFT_IDLE)
    }
}

#[derive(Default)]
struct Counted(usize);

impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl fmt::Write for Counted {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 += text.len();
        Ok(())
    }
}

pub(super) fn json_size(value: &impl serde::Serialize) -> Result<usize, DeskError> {
    let mut counted = Counted::default();
    serde_json::to_writer(&mut counted, value).map_err(super::failed)?;
    Ok(counted.0)
}

pub(super) fn debug_size(value: &impl fmt::Debug) -> Result<usize, DeskError> {
    let mut counted = Counted::default();
    fmt::write(&mut counted, format_args!("{value:?}")).map_err(super::failed)?;
    Ok(counted.0)
}

pub(super) fn held_bytes(
    sealed: &Sealed,
    local: Option<&LocalReview>,
    report: &Report,
) -> Result<usize, DeskError> {
    let local = match local {
        Some(local) => json_size(local)?,
        None => 0,
    };
    Ok(2 * sealed.text.len() + local + debug_size(report)?)
}

use jiff::Timestamp;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;

use super::Quota;
use crate::drops::DropId;
use crate::tenancy::UserId;

#[derive(Debug)]
pub(super) struct Entry {
    pub(super) owner: UserId,
    pub(super) bytes: u64,
    pub(super) expires_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fate {
    Send,
    Burn,
}

#[derive(Debug, Default)]
pub(super) struct Index {
    pub(super) live: HashMap<DropId, Entry>,
    pub(super) reserved: HashMap<DropId, (UserId, u64)>,
    pub(super) claimed: HashMap<DropId, Fate>,
}

impl Index {
    pub(super) fn holds(&self, id: &DropId) -> bool {
        self.live.contains_key(id) || self.reserved.contains_key(id)
    }

    pub(super) fn usage(&self, owner: &UserId) -> (u64, u64) {
        let live = self
            .live
            .values()
            .filter(|entry| entry.owner == *owner)
            .map(|entry| entry.bytes);
        let reserved = self
            .reserved
            .values()
            .filter(|(holder, _)| holder == owner)
            .map(|(_, bytes)| *bytes);
        live.chain(reserved)
            .fold((0, 0), |(count, total), bytes| (count + 1, total + bytes))
    }

    pub(super) fn fits(&self, owner: &UserId, bytes: u64, quota: Quota) -> bool {
        let (count, total) = self.usage(owner);
        count < quota.drops && total.saturating_add(bytes) <= quota.bytes
    }

    pub(super) fn release_taken_elsewhere(
        &mut self,
        owner: &UserId,
        record: impl Fn(&DropId) -> PathBuf,
    ) {
        let Self { live, claimed, .. } = self;
        live.retain(|id, entry| {
            entry.owner != *owner
                || claimed.contains_key(id)
                || !matches!(
                    std::fs::symlink_metadata(record(id)),
                    Err(error) if error.kind() == ErrorKind::NotFound
                )
        });
    }
}

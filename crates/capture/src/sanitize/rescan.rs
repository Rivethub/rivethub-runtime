use std::num::NonZeroUsize;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;

use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureMessage;

use super::key::LocalKey;
use super::mapping::Mapping;
use super::report::Change;
use super::report::Collision;
use super::scrub::Scrubber;
use crate::rules::Budget;

#[derive(Debug, Default)]
pub(super) struct Found {
    pub(super) changes: Vec<Change>,
    pub(super) collisions: Vec<Collision>,
}

#[derive(Debug, Clone, Copy)]
enum Unit {
    Header(usize),
    Message(usize, usize),
}

enum Output {
    Header(usize, BundledSession),
    Message(usize, usize, CaptureMessage),
}

struct Scanned {
    output: Output,
    found: Found,
    spent: u64,
}

pub(super) struct Rescan<'a> {
    pub(super) key: Option<&'a LocalKey>,
    pub(super) digest: &'a str,
    pub(super) whole: bool,
}

impl Rescan<'_> {
    pub(super) fn run(
        &self,
        sessions: &mut [BundledSession],
        exported_by: &mut BundleAuthor,
    ) -> Found {
        let mapping = Mapping::default();
        let heads: Vec<BundledSession> = sessions.iter_mut().map(header_only).collect();
        let units: Vec<Unit> = sessions
            .iter()
            .enumerate()
            .flat_map(|(index, session)| {
                std::iter::once(Unit::Header(index)).chain(
                    (0..session.batch.messages.len()).map(move |at| Unit::Message(index, at)),
                )
            })
            .collect();
        let scanned = parallel(&units, |unit| self.scan(&mapping, &heads, sessions, unit));
        let mut found = Found::default();
        let mut spent = 0;
        let mut settled = 0;
        for scanned in scanned {
            let Some(scanned) = scanned.filter(|scanned| Budget::leaves_room(spent, scanned.spent))
            else {
                break;
            };
            spent += scanned.spent;
            settled += 1;
            put(sessions, scanned.output);
            found.changes.extend(scanned.found.changes);
            found.collisions.extend(scanned.found.collisions);
        }
        let mut scrubber = self.scrubber(&mapping, Budget::after(spent));
        for &unit in units.iter().skip(settled) {
            match unit {
                Unit::Header(index) => {
                    if let Some(session) = sessions.get_mut(index) {
                        scrubber.header(index, session);
                    }
                }
                Unit::Message(index, at) => {
                    if let Some(message) = message_mut(sessions, index, at) {
                        scrub_message(&mut scrubber, index, at, message);
                    }
                }
            }
        }
        scrubber.manifest(exported_by);
        found.changes.append(&mut scrubber.changes);
        found.collisions.append(&mut scrubber.collisions);
        found
    }

    fn scan(
        &self,
        mapping: &Mapping,
        heads: &[BundledSession],
        sessions: &[BundledSession],
        unit: Unit,
    ) -> Option<Scanned> {
        let mut scrubber = self.scrubber(mapping, Budget::after(0));
        let output = match unit {
            Unit::Header(index) => {
                let mut head = heads.get(index)?.clone();
                scrubber.header(index, &mut head);
                Output::Header(index, head)
            }
            Unit::Message(index, at) => {
                let mut message = sessions.get(index)?.batch.messages.get(at)?.clone();
                scrub_message(&mut scrubber, index, at, &mut message);
                Output::Message(index, at, message)
            }
        };
        Some(Scanned {
            output,
            spent: scrubber.budget.spent(),
            found: Found {
                changes: scrubber.changes,
                collisions: scrubber.collisions,
            },
        })
    }

    fn scrubber<'s>(&'s self, mapping: &'s Mapping, budget: Budget) -> Scrubber<'s> {
        let mut scrubber = Scrubber::new(mapping, self.key, self.digest);
        scrubber.whole = self.whole;
        scrubber.budget = budget;
        scrubber
    }
}

fn put(sessions: &mut [BundledSession], output: Output) {
    match output {
        Output::Header(index, head) => {
            if let Some(session) = sessions.get_mut(index) {
                let messages = std::mem::take(&mut session.batch.messages);
                *session = head;
                session.batch.messages = messages;
            }
        }
        Output::Message(index, at, message) => {
            if let Some(slot) = message_mut(sessions, index, at) {
                *slot = message;
            }
        }
    }
}

fn message_mut(
    sessions: &mut [BundledSession],
    index: usize,
    at: usize,
) -> Option<&mut CaptureMessage> {
    sessions
        .get_mut(index)
        .and_then(|session| session.batch.messages.get_mut(at))
}

fn header_only(session: &mut BundledSession) -> BundledSession {
    let messages = std::mem::take(&mut session.batch.messages);
    let head = session.clone();
    session.batch.messages = messages;
    head
}

fn scrub_message(
    scrubber: &mut Scrubber<'_>,
    index: usize,
    at: usize,
    message: &mut CaptureMessage,
) {
    let start = scrubber.changes.len();
    let collisions = scrubber.collisions.len();
    scrubber.message(index, at, message);
    for change in scrubber.changes.iter_mut().skip(start) {
        change.event_id = Some(message.event_id.clone());
    }
    for collision in scrubber.collisions.iter_mut().skip(collisions) {
        collision.event_id = Some(message.event_id.clone());
    }
}

fn parallel<F>(units: &[Unit], work: F) -> Vec<Option<Scanned>>
where
    F: Fn(Unit) -> Option<Scanned> + Sync,
{
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): when the OS cannot report its cores the scan runs on one thread, with the same result"
        )
    )]
    let cores = thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let threads = cores.min(units.len().max(1));
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Option<Scanned>)> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&unit) = units.get(index) else {
                            return done;
                        };
                        done.push((index, work(unit)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, scanned)| scanned).collect()
}

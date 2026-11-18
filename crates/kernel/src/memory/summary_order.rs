use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;
use values::{JsonObject, Role};

use super::MemoryBrowseOrder;

#[derive(Debug, Clone, Copy)]
pub struct MessagePosition<'a> {
    stamp: DateTime<Utc>,
    role: Role,
    metadata: Option<&'a JsonObject>,
    conversation: Option<Uuid>,
    path: Option<&'a str>,
    line: Option<u64>,
}

impl<'a> MessagePosition<'a> {
    pub fn new(stamp: DateTime<Utc>, role: Role, metadata: Option<&'a JsonObject>) -> Self {
        let path = text(metadata, "session_jsonl_path");
        let line = metadata
            .and_then(|object| object.get("session_jsonl_line"))
            .and_then(Value::as_u64);
        Self {
            stamp,
            role,
            metadata,
            conversation: None,
            path,
            line,
        }
    }

    #[must_use]
    pub const fn in_conversation(mut self, conversation: Uuid) -> Self {
        self.conversation = Some(conversation);
        self
    }

    #[must_use]
    pub const fn has_line(self) -> bool {
        self.line.is_some()
    }

    fn line_key(self) -> (Option<u64>, Option<u64>, u8) {
        let ordinal = self
            .metadata
            .and_then(|object| object.get("ordinal"))
            .and_then(Value::as_u64);
        let role = match self.role {
            Role::Assistant => 0,
            Role::Tool => 2,
            Role::System | Role::User => 1,
        };
        (self.line, ordinal, role)
    }

    fn correlation(self) -> [Option<Correlation<'a>>; 3] {
        let event = text(self.metadata, "event_id")
            .filter(|event| self.role == Role::Tool && tool_call(event).is_some());
        let call = text(self.metadata, "tool_use_id")
            .or_else(|| event.filter(|_| self.line.is_some()).and_then(tool_call));
        let uuid = text(self.metadata, "uuid");
        let turn = if self.line.is_some() {
            uuid.filter(|_| self.role == Role::Assistant)
        } else {
            text(self.metadata, "assistant_uuid")
                .or_else(|| uuid.filter(|_| self.role == Role::Tool))
        };
        [
            event.map(Correlation::Event),
            call.map(Correlation::Call),
            turn.map(Correlation::Turn),
        ]
    }
}

fn text<'a>(metadata: Option<&'a JsonObject>, key: &str) -> Option<&'a str> {
    metadata?.get(key)?.as_str().filter(|text| !text.is_empty())
}

fn tool_call(event: &str) -> Option<&str> {
    let (session, call) = event.rsplit_once(":tool:")?;
    (!session.is_empty() && !call.is_empty()).then_some(call)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Correlation<'a> {
    Event(&'a str),
    Call(&'a str),
    Turn(&'a str),
}

type Stream<'a> = (Option<Uuid>, Option<&'a str>);
type AnchorKey<'a> = (Stream<'a>, Correlation<'a>);
type TimeKey = (DateTime<Utc>, usize);
type Positioned<'a> = (MessagePosition<'a>, usize);

#[derive(Clone, Copy)]
enum Anchor {
    Unique(usize),
    Ambiguous,
}

fn anchors<'a>(rows: &[Positioned<'a>]) -> HashMap<AnchorKey<'a>, Anchor> {
    let mut anchors = HashMap::new();
    for &(position, index) in rows.iter().filter(|(position, _)| position.line.is_some()) {
        for correlation in position.correlation().into_iter().flatten() {
            for path in std::iter::once(None).chain(position.path.map(Some)) {
                anchors
                    .entry(((position.conversation, path), correlation))
                    .and_modify(|anchor| *anchor = Anchor::Ambiguous)
                    .or_insert(Anchor::Unique(index));
            }
        }
    }
    anchors
}

fn partition(rows: Vec<Positioned<'_>>) -> (Vec<Vec<TimeKey>>, HashMap<usize, Vec<TimeKey>>) {
    let anchors = anchors(&rows);
    let mut streams = BTreeMap::<Stream<'_>, Vec<Positioned<'_>>>::new();
    let mut fallback = Vec::new();
    let mut attached = HashMap::<usize, Vec<TimeKey>>::new();
    for (position, index) in rows {
        let stream = (position.conversation, position.path);
        let time = (position.stamp, index);
        if position.line.is_some() {
            streams.entry(stream).or_default().push((position, index));
            continue;
        }
        // A matching ID kind decides the attachment, even when its match is ambiguous.
        let anchor = position
            .correlation()
            .into_iter()
            .flatten()
            .find_map(|key| anchors.get(&(stream, key)));
        if let Some(Anchor::Unique(anchor)) = anchor {
            attached.entry(*anchor).or_default().push(time);
        } else {
            fallback.push(time);
        }
    }
    fallback.sort_unstable();
    let streams = streams.into_values().map(|mut stream| {
        stream.sort_unstable_by_key(|(position, index)| {
            (position.line_key(), position.stamp, *index)
        });
        stream
            .into_iter()
            .map(|(position, index)| (position.stamp, index))
            .collect()
    });
    (streams.chain([fallback]).collect(), attached)
}

pub fn order_messages<T>(
    legacy_rows: Vec<T>,
    direction: MemoryBrowseOrder,
    position: impl Fn(&T) -> MessagePosition<'_>,
) -> Vec<T> {
    let positions: Vec<_> = legacy_rows.iter().map(position).zip(0..).collect();
    if positions
        .iter()
        .all(|(position, _)| position.line.is_none())
    {
        return legacy_rows;
    }
    let (streams, attached) = partition(positions);
    let mut order = merge(streams, attached);
    if direction == MemoryBrowseOrder::Desc {
        order.reverse();
    }
    let mut rows: Vec<_> = legacy_rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| rows.get_mut(index).and_then(Option::take))
        .collect()
}

fn merge(streams: Vec<Vec<TimeKey>>, mut attached: HashMap<usize, Vec<TimeKey>>) -> Vec<usize> {
    let mut streams: Vec<_> = streams.into_iter().map(Vec::into_iter).collect();
    let mut heads: BinaryHeap<_> = streams
        .iter_mut()
        .enumerate()
        .filter_map(|(stream, rows)| Some(Reverse((rows.next()?, stream))))
        .collect();
    let mut order = Vec::new();
    while let Some(Reverse(((_, index), stream))) = heads.pop() {
        order.push(index);
        if let Some(mut cluster) = attached.remove(&index) {
            cluster.sort_unstable();
            order.extend(cluster.into_iter().map(|(_, index)| index));
        }
        if let Some(next) = streams.get_mut(stream).and_then(Iterator::next) {
            heads.push(Reverse((next, stream)));
        }
    }
    order
}

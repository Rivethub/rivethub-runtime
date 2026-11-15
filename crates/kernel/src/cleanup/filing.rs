use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::BuildHasher;

use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use super::Automated;
use super::Lift;
use super::ProposedTree;
use super::Snapshot;
use super::SnapshotConversation;
use super::SnapshotHead;
use crate::filing::Evidence;
use crate::filing::Filable;
use crate::filing::Refusal;
use crate::filing::Rule;
use crate::filing::Step;
use crate::filing::Target;
use crate::topics::PlacementSource;
use crate::topics::Redirect;
use crate::topics::TopicId;
use crate::topics::TopicKind;
use crate::topics::TopicListing;
use crate::topics::generic_keys;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "kebab-case")]
pub enum FiledHow {
    Never,
    SameTarget,
    Placed { by: PlacementSource },
    Corrected,
    Inherits,
    Rule { rule: Rule },
    Suggested { name: String },
    Refused { name: String, refusal: Refusal },
    Nothing,
    Unanswered,
    Automated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filing {
    pub conversation: Uuid,
    pub topic: Option<TopicId>,
    #[serde(flatten)]
    pub how: FiledHow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifted: Option<String>,
}

impl Filing {
    #[must_use]
    pub const fn new(conversation: Uuid, topic: Option<TopicId>, how: FiledHow) -> Self {
        Self {
            conversation,
            topic,
            how,
            lifted: None,
        }
    }

    pub fn lift(&mut self, lifts: &[Lift]) {
        for _ in 0..lifts.len() {
            let Some(lift) = lifts.iter().find(|lift| Some(lift.from) == self.topic) else {
                break;
            };
            self.topic = lift.to;
            self.lifted.get_or_insert_with(|| lift.name.clone());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    Done(Filing),
    Ask { named: Option<TopicId> },
}

#[derive(Debug, Clone)]
pub struct Desk {
    listings: Vec<TopicListing>,
    redirects: Vec<Redirect>,
    placed: HashMap<Target, (PlacementSource, TopicId)>,
    corrected: HashSet<Target>,
    generic: BTreeSet<String>,
    automated: Automated,
}

impl Desk {
    #[must_use]
    pub fn new(
        snapshot: &Snapshot,
        tree: &ProposedTree,
        listings: Vec<TopicListing>,
        filable: &[&SnapshotConversation],
        automated: Automated,
    ) -> Self {
        let work: HashSet<TopicId> = snapshot
            .topics
            .iter()
            .filter(|topic| topic.kind == TopicKind::Work)
            .map(|topic| topic.id)
            .collect();
        let placed = snapshot
            .placements
            .iter()
            .filter(|placement| work.contains(&placement.topic))
            .filter_map(|placement| {
                Some((
                    placement.target()?,
                    (placement.by, tree.resolve(placement.topic)?),
                ))
            })
            .collect();
        let corrected = snapshot
            .corrections
            .iter()
            .filter_map(super::SnapshotCorrection::target)
            .collect();
        let redirects = tree.redirects(snapshot);
        let corpus: Vec<Vec<&str>> = filable
            .iter()
            .map(|conversation| {
                conversation
                    .title()
                    .into_iter()
                    .chain(conversation.summary.as_deref())
                    .collect()
            })
            .collect();
        let generic = generic_keys(&listings, &redirects, &corpus);
        Self {
            listings,
            redirects,
            placed,
            corrected,
            generic,
            automated,
        }
    }

    #[must_use]
    pub const fn generic(&self) -> &BTreeSet<String> {
        &self.generic
    }

    #[must_use]
    pub fn parent_session(conversation: &SnapshotConversation) -> Option<Target> {
        let parent = conversation.parent_session.as_deref()?;
        Target::stored(Some(parent), None)
    }

    #[must_use]
    pub fn listings(&self) -> &[TopicListing] {
        &self.listings
    }

    #[must_use]
    pub fn target(conversation: &SnapshotConversation) -> Option<Target> {
        Filable::of(&conversation.head.head()).target(conversation.head.id)
    }

    #[must_use]
    pub fn is_task(conversation: &SnapshotConversation) -> bool {
        matches!(Filable::of(&conversation.head.head()), Filable::Task(_))
    }

    #[must_use]
    pub fn turn(
        &self,
        conversation: &SnapshotConversation,
        target: &Target,
        parent_topic: Option<TopicId>,
    ) -> Turn {
        let placed = self.placed.get(target).copied();
        let id = conversation.head.id;
        if placed.is_none() && self.automated.runs.contains(&id) {
            return Turn::Done(Filing::new(id, None, FiledHow::Automated));
        }
        let evidence = Evidence {
            placed: placed.map(|(by, _)| by),
            corrected: self.corrected.contains(target),
            child: conversation.parent_session.is_some(),
            parent_topic,
            cwd: conversation.cwd.as_deref(),
            title: conversation.title(),
            summary: conversation.summary.as_deref(),
            generic: &self.generic,
        };
        match evidence.decide(&self.listings, &self.redirects) {
            Step::Placed(by) => Turn::Done(Filing::new(
                id,
                placed.map(|(_, topic)| topic),
                FiledHow::Placed { by },
            )),
            Step::Corrected => Turn::Done(Filing::new(id, None, FiledHow::Corrected)),
            Step::Inherits => Turn::Done(Filing::new(id, None, FiledHow::Inherits)),
            Step::File { topic, rule } => {
                Turn::Done(Filing::new(id, Some(topic), FiledHow::Rule { rule }))
            }
            Step::Ask { named } => Turn::Ask { named },
        }
    }

    #[must_use]
    pub fn parent_topic<S: BuildHasher>(
        &self,
        conversation: &SnapshotConversation,
        filed: &HashMap<Target, Option<TopicId>, S>,
    ) -> Option<TopicId> {
        let parent: &SnapshotHead = conversation.task_parent.as_ref()?;
        let target = Filable::of(&parent.head()).target(parent.id)?;
        filed
            .get(&target)
            .copied()
            .flatten()
            .or_else(|| self.placed.get(&target).map(|(_, topic)| *topic))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grouped<'a> {
    pub filable: Vec<(Target, &'a SnapshotConversation)>,
    pub same: Vec<(Target, &'a SnapshotConversation)>,
    pub never: Vec<&'a SnapshotConversation>,
}

#[must_use]
pub fn group(conversations: &[SnapshotConversation]) -> Grouped<'_> {
    let mut latest: HashMap<Target, &SnapshotConversation> = HashMap::new();
    let mut grouped = Grouped::default();
    for conversation in conversations {
        let Some(target) = Desk::target(conversation) else {
            grouped.never.push(conversation);
            continue;
        };
        let kept = latest.entry(target.clone()).or_insert(conversation);
        if (conversation.last_active, conversation.head.id) > (kept.last_active, kept.head.id) {
            grouped.same.push((target, *kept));
            *kept = conversation;
        } else if kept.head.id != conversation.head.id {
            grouped.same.push((target, conversation));
        }
    }
    grouped.filable = latest.into_iter().collect();
    grouped
        .filable
        .sort_by_key(|(_, conversation)| (conversation.last_active, conversation.head.id));
    grouped
}

#[must_use]
pub fn filed_counts(filings: &[Filing]) -> BTreeMap<TopicId, u32> {
    let mut counts = BTreeMap::new();
    for topic in filings
        .iter()
        .filter(|filing| filing.how != FiledHow::SameTarget)
        .filter_map(|filing| filing.topic)
    {
        let count: &mut u32 = counts.entry(topic).or_default();
        *count = count.saturating_add(1);
    }
    counts
}

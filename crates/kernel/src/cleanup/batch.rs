use jiff::Timestamp;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;

use uuid::Uuid;

use super::Desk;
use super::Fate;
use super::FiledHow;
use super::Filing;
use super::Folded;
use super::Origin;
use super::ProposedTree;
use super::Snapshot;
use crate::filing::Target;
use crate::tidy::Batch;
use crate::tidy::Evidence;
use crate::tidy::Proposer;
use crate::tidy::Reason;
use crate::tidy::Sample;
use crate::tidy::TidyError;
use crate::tidy::TreeEdit;
use crate::topics::Alias;
use crate::topics::TopicId;
use crate::topics::TopicName;
use crate::topics::fold;
use crate::wiki::page::WikiSlug;

#[derive(Debug, Clone, Copy)]
pub struct BackfillBatch<'a> {
    pub snapshot: &'a Snapshot,
    pub tree: &'a ProposedTree,
    pub filings: &'a [Filing],
    pub pages: &'a [Folded],
    pub generic: &'a BTreeSet<String>,
}

impl BackfillBatch<'_> {
    pub fn batch(&self, at: Timestamp) -> Result<Batch, TidyError> {
        let targets: HashMap<Uuid, Target> = self
            .snapshot
            .conversations
            .iter()
            .filter_map(|conversation| {
                Desk::target(conversation).map(|target| (conversation.head.id, target))
            })
            .collect();
        let seen: HashMap<Uuid, (Option<&str>, Timestamp)> = self
            .snapshot
            .conversations
            .iter()
            .map(|conversation| {
                (
                    conversation.head.id,
                    (conversation.title(), conversation.last_active),
                )
            })
            .collect();
        let mut members: BTreeMap<TopicId, Vec<(Target, Uuid)>> = BTreeMap::new();
        for filing in self.filings {
            if let (Some(topic), FiledHow::Rule { .. }, Some(target)) =
                (filing.topic, &filing.how, targets.get(&filing.conversation))
            {
                members
                    .entry(topic)
                    .or_default()
                    .push((target.clone(), filing.conversation));
            }
        }
        let mut batch = Batch::new(Proposer::Backfill, at);
        for topic in &self.tree.topics {
            let filed = members.remove(&topic.id).unwrap_or_default();
            let evidence = evidence(&filed, &seen)?;
            let members: Vec<Target> = filed.into_iter().map(|(target, _)| target).collect();
            let edits = self.topic_edits(topic, members)?;
            for (edit, reason) in edits {
                batch.add(edit, Reason::new(&reason)?, evidence.clone());
            }
        }
        for merge in &self.tree.merges {
            if let Some(into) = self.tree.resolve(merge.into) {
                batch.add(
                    TreeEdit::Merge {
                        source: merge.from,
                        into,
                    },
                    Reason::new(&format!(
                        "{} is the same area as {}",
                        merge.name,
                        self.tree.path(into)
                    ))?,
                    Evidence::default(),
                );
            }
        }
        for page in self.pages {
            if let Some((edit, reason)) = page_edit(page)? {
                batch.add(edit, Reason::new(&reason)?, Evidence::default());
            }
        }
        Ok(batch)
    }

    fn topic_edits(
        &self,
        topic: &super::TreeTopic,
        members: Vec<Target>,
    ) -> Result<Vec<(TreeEdit, String)>, TidyError> {
        let count = members.len();
        let path = self.tree.path(topic.id);
        if topic.origin != Origin::Existing {
            let aliases = topic
                .aliases
                .iter()
                .filter(|alias| !self.generic.contains(&fold(alias)))
                .map(|alias| Alias::new(alias))
                .collect::<Result<_, _>>()?;
            let reason = match topic.origin {
                Origin::Project => {
                    format!("{path} is one of your projects; {count} sessions file under it")
                }
                Origin::Existing | Origin::Proposed => {
                    format!("{count} sessions are about {path}")
                }
            };
            return Ok(vec![(
                TreeEdit::CreateTopic {
                    topic: topic.id,
                    name: TopicName::new(&topic.name)?,
                    parent: topic.parent,
                    aliases,
                    members,
                },
                reason,
            )]);
        }
        let mut edits = Vec::new();
        let stored = self
            .snapshot
            .topics
            .iter()
            .find(|existing| existing.id == topic.id);
        if let Some(stored) = stored {
            if stored.parent != topic.parent {
                edits.push((
                    TreeEdit::Move {
                        topic: topic.id,
                        parent: topic.parent,
                    },
                    format!("{} belongs under {path}", stored.name),
                ));
            }
            if stored.name != topic.name {
                edits.push((
                    TreeEdit::Rename {
                        topic: topic.id,
                        name: TopicName::new(&topic.name)?,
                    },
                    format!("{} is written {}", stored.name, topic.name),
                ));
            }
        }
        if !members.is_empty() {
            edits.push((
                TreeEdit::FileInto {
                    topic: topic.id,
                    members,
                },
                format!("{count} sessions are about {path}"),
            ));
        }
        Ok(edits)
    }
}

fn page_edit(page: &Folded) -> Result<Option<(TreeEdit, String)>, TidyError> {
    let slug = |text: &str| {
        WikiSlug::try_from(text.to_owned()).map_err(|_| TidyError::Invalid {
            what: text.to_owned(),
        })
    };
    Ok(match &page.fate {
        Fate::Merge { into } if into.contains('#') => None,
        Fate::Merge { into } => Some((
            TreeEdit::MergePage {
                page: slug(&page.key)?,
                into: slug(into)?,
            },
            format!("{} covers the same thing as {into}", page.title),
        )),
        Fate::Remove => Some((
            TreeEdit::RemovePage {
                page: slug(&page.key)?,
            },
            format!("{} is a test, temporary or empty page", page.title),
        )),
        Fate::Entity { .. }
        | Fate::Topic { .. }
        | Fate::Section { .. }
        | Fate::Keep
        | Fate::Undecided => None,
    })
}

fn evidence(
    filed: &[(Target, Uuid)],
    seen: &HashMap<Uuid, (Option<&str>, Timestamp)>,
) -> Result<Evidence, TidyError> {
    Ok(Evidence {
        sessions: u64::try_from(filed.len()).map_err(|_| TidyError::Invalid {
            what: "a session count".to_owned(),
        })?,
        samples: filed
            .iter()
            .map(|(target, conversation)| {
                let (title, last_active) = seen
                    .get(conversation)
                    .map_or((None, None), |(title, at)| (*title, Some(*at)));
                Sample {
                    id: match target {
                        Target::Session(session) => session.as_str().to_owned(),
                        Target::Conversation(id) => id.to_string(),
                    },
                    title: title.map(str::to_owned),
                    last_active,
                }
            })
            .collect(),
    })
}

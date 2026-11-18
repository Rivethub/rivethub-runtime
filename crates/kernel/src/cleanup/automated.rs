use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use super::SnapshotConversation;
use super::TaskOrigin;
use crate::topics::fold;

pub const TEMPLATE_RUNS: usize = 20;
const TEMPLATE_CHARS: usize = 80;
const MIN_PROMPT_CHARS: usize = 60;
const SINGLE_TURN_PERCENT: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub opening: String,
    pub runs: usize,
    pub left_out: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automated {
    pub runs: BTreeSet<Uuid>,
    pub by_task: usize,
    pub templates: Vec<Template>,
}

#[must_use]
pub fn seed_keys(snapshot: &super::Snapshot) -> BTreeSet<String> {
    snapshot
        .projects
        .iter()
        .flat_map(|project| std::iter::once(&project.name).chain(&project.aliases))
        .chain(
            snapshot
                .topics
                .iter()
                .filter(|topic| topic.kind == crate::topics::TopicKind::Work)
                .map(|topic| &topic.name),
        )
        .map(|name| fold(name))
        .filter(|key| !key.is_empty())
        .collect()
}

impl Automated {
    #[must_use]
    pub fn detect<'a>(
        conversations: impl IntoIterator<Item = &'a SnapshotConversation>,
        project_keys: &BTreeSet<String>,
    ) -> Self {
        let mut automated = Self::default();
        let mut groups: BTreeMap<String, Vec<&SnapshotConversation>> = BTreeMap::new();
        for conversation in conversations {
            if matches!(
                conversation.task_origin,
                Some(TaskOrigin::Heartbeat | TaskOrigin::Eval)
            ) {
                automated.runs.insert(conversation.head.id);
                automated.by_task = automated.by_task.saturating_add(1);
                continue;
            }
            if let Some(key) = conversation.opening.as_deref().and_then(template_key) {
                groups.entry(key).or_default().push(conversation);
            }
        }
        for (opening, runs) in groups {
            if runs.len() < TEMPLATE_RUNS {
                continue;
            }
            let single = runs
                .iter()
                .filter(|conversation| conversation.user_turns <= 1)
                .count();
            if single.saturating_mul(100) < runs.len().saturating_mul(SINGLE_TURN_PERCENT) {
                continue;
            }
            let left_out: Vec<Uuid> = runs
                .iter()
                .filter(|conversation| !names_work(conversation, project_keys))
                .map(|conversation| conversation.head.id)
                .collect();
            if left_out.is_empty() {
                continue;
            }
            automated.templates.push(Template {
                opening,
                runs: runs.len(),
                left_out: left_out.len(),
            });
            automated.runs.extend(left_out);
        }
        automated
            .templates
            .sort_by_key(|template| std::cmp::Reverse(template.left_out));
        automated
    }
}

fn template_key(opening: &str) -> Option<String> {
    let letters: String = opening
        .chars()
        .map(|ch| {
            if ch.is_alphabetic() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = letters.split_whitespace().collect();
    let joined = words.join(" ");
    (joined.chars().count() >= MIN_PROMPT_CHARS)
        .then(|| joined.chars().take(TEMPLATE_CHARS).collect())
}

fn names_work(conversation: &SnapshotConversation, project_keys: &BTreeSet<String>) -> bool {
    let Some(opening) = conversation.opening.as_deref() else {
        return false;
    };
    let folded = format!("-{}-", fold(opening));
    project_keys
        .iter()
        .any(|key| folded.contains(&format!("-{key}-")))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::cleanup::SnapshotHead;
    use jiff::Timestamp;

    fn conversation(opening: &str, user_turns: u32) -> SnapshotConversation {
        SnapshotConversation {
            head: SnapshotHead {
                id: Uuid::new_v4(),
                session_key: format!("claude-code:{}", Uuid::new_v4()),
                agent: "agent".to_owned(),
                channel: "claude-code".to_owned(),
                title: None,
            },
            cwd: None,
            summary: None,
            last_active: Timestamp::now(),
            task_parent: None,
            task_origin: None,
            opening: Some(opening.to_owned()),
            user_turns,
            parent_session: None,
        }
    }

    #[test]
    fn repeated_single_turn_prompts_that_name_no_project_are_left_out() {
        let check = "Scheduled check: read the checklist, look at mail, calendar and the weather, then report.";
        let review =
            "You are a reviewer for AcmeKit: read the pull request and report what must change.";
        let mut conversations: Vec<SnapshotConversation> = (0..TEMPLATE_RUNS)
            .map(|run| conversation(&format!("{check} Run {run}."), 1))
            .chain((0..TEMPLATE_RUNS).map(|_| conversation(review, 1)))
            .chain((0..TEMPLATE_RUNS).map(|_| conversation("hey", 1)))
            .collect();
        let mut evaluated = conversation("Grade the answer.", 1);
        evaluated.task_origin = Some(TaskOrigin::Eval);
        conversations.push(evaluated);
        let keys = BTreeSet::from(["acmekit".to_owned()]);
        let automated = Automated::detect(&conversations, &keys);
        assert_eq!(automated.runs.len(), TEMPLATE_RUNS + 1);
        assert_eq!(automated.by_task, 1);
        assert_eq!(automated.templates.len(), 1);
    }
}

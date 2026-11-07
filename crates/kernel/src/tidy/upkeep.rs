use chrono::DateTime;
use chrono::Utc;

use crate::queue::WorkerRole;
use crate::topics::TopicId;
use crate::topics::TopicName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilingTally {
    pub categorizer: u64,
    pub you: u64,
    pub uncategorized: u64,
    pub left_out: u64,
    pub corrections: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTally {
    pub topic: TopicId,
    pub name: TopicName,
    pub entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiTally {
    pub topic_pages: u64,
    pub types: Vec<TypeTally>,
    pub loose_pages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskActivity {
    pub task: String,
    pub last_finished: Option<DateTime<Utc>>,
    pub failing: u64,
    pub queued: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleActivity {
    pub last_finished: Option<DateTime<Utc>>,
    pub failing: u64,
    pub queued: u64,
}

impl RoleActivity {
    #[must_use]
    pub fn of(role: WorkerRole, tasks: &[TaskActivity]) -> Self {
        let names: Vec<String> = role.task_list().iter().map(ToString::to_string).collect();
        tasks
            .iter()
            .filter(|activity| names.contains(&activity.task))
            .fold(Self::default(), |role, activity| Self {
                last_finished: role.last_finished.max(activity.last_finished),
                failing: role.failing + activity.failing,
                queued: role.queued + activity.queued,
            })
    }
}

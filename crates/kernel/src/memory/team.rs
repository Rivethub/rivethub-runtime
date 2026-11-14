use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSession {
    pub owner: String,
    pub session_id: String,
    pub title: String,
    pub when: DateTime<Utc>,
    pub topic: Vec<String>,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallIdentity {
    pub principal: Option<String>,
    pub people: Vec<String>,
}

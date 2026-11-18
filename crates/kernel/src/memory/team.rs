use jiff::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSession {
    pub owner: String,
    pub session_id: String,
    pub title: String,
    pub when: Timestamp,
    pub topic: Vec<String>,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallIdentity {
    pub principal: Option<String>,
    pub people: Vec<String>,
}

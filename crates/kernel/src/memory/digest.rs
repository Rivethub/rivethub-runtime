use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDigest {
    pub key: String,
    pub conversation_id: Uuid,
    pub title: Option<String>,
    pub agent: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub messages: u64,
    pub imported_from: Option<String>,
}

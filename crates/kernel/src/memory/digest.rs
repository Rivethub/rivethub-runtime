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

pub const UNKNOWN_SENDER: &str = "unknown sender";

impl SessionDigest {
    #[must_use]
    pub fn imported_author(&self) -> Option<String> {
        super::is_imported(&self.key).then(|| {
            self.imported_from
                .as_deref()
                .and_then(super::display_author)
                .unwrap_or_else(|| UNKNOWN_SENDER.to_owned())
        })
    }
}

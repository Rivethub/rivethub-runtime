use super::TagEntityType;
use super::TagState;
use jiff::Timestamp;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagVocabulary {
    pub key: String,
    pub value: String,
    pub display: String,
    pub aliases: Vec<String>,
    pub state: TagState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagOnEntity {
    pub entity_type: TagEntityType,
    pub key: String,
    pub value: String,
    pub display: String,
    pub source: String,
    pub state: TagState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagJobContext {
    pub summary_id: Uuid,
    pub conversation_id: Uuid,
    pub session_key: String,
    pub title: Option<String>,
    pub agent: String,
    pub summary: String,
    pub vocabulary: Vec<TagVocabulary>,
    pub tags: Vec<TagOnEntity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSuggestKind {
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagSuggestionRow {
    pub kind: TagSuggestKind,
    pub entity_type: TagEntityType,
    pub entity_id: Uuid,
    pub key: String,
    pub value: String,
    pub display: String,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagVocabularyProposal {
    pub key: String,
    pub value: String,
    pub display: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagSuggestionWrite {
    pub proposed_by: String,
    pub at: Timestamp,
    pub rows: Vec<TagSuggestionRow>,
    pub vocabulary: Vec<TagVocabularyProposal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagSuggestionStored {
    pub tags: u64,
    pub removals: u64,
    pub vocabulary: u64,
}

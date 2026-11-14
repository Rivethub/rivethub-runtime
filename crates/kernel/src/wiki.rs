pub mod builder;
pub mod consolidate;
pub mod entity;
pub mod entity_admit;
pub mod entity_match;
pub mod entity_pass;
pub mod entity_vocab;
pub mod merge;
pub mod origin;
pub mod page;
pub mod recompile;
pub mod route;
mod view;

pub use view::{
    WikiCitation, WikiGapsResponse, WikiPageResponse, WikiRedLink, WikiSource, WikiSourceSpan,
    WikiTopic, WikiTopicList,
};

pub const WIKI_LIMIT_MAX: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiFilter {
    pub query: Option<String>,
    pub tag: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiLookup {
    Page(Box<WikiPageResponse>),
    Redirect(String),
    Missing,
}

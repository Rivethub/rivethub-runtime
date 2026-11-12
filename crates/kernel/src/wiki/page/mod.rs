pub mod archive;
mod article;
pub mod bound;
mod citations;
mod extraction;
mod fold;
mod frontmatter;
mod identity;
mod imported;
pub mod lead;
mod limits;
mod lines;
mod parse;
mod patch;
mod read;
mod search;
mod serialize;
mod slug;
mod summary;
mod yaml;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use values::Extra;

pub use article::{
    ArticlePatch, ArticlePatchMode, ArticleSection, ArticleSections, apply_article_patches,
    demote_h2_headings, join_article_sections, split_article_sections,
};
pub use citations::{parse_citations, parse_see_also};
pub use extraction::{PatchFact, WikiAction, WikiPatch};
pub use fold::{FoldMark, FoldedTopic, fold_page, fold_text};
pub use identity::{TopicIdentity, find_stem_match, is_slug_variant, topic_identity};
pub use imported::{imported_banner, mark_imported_citations};
pub use parse::parse_wiki_page;
pub use patch::{PagePatch, apply_patch};
pub use read::{FullPageRefused, READ_LIMIT, ReadSection, format_wiki_read};
pub use search::build_wiki_search_text;
pub use serialize::serialize_wiki_page;
pub use slug::{WikiSlug, WikiSlugError, extract_wiki_links, normalize_slug};
pub use summary::{CappedSummary, SUMMARY_LIMIT, cap_summary, merge_summary_text};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiPage {
    pub meta: WikiMeta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
    pub current_state: String,
    pub article: String,
    pub history: Vec<WikiHistoryEntry>,
    pub citations: Vec<PageCitation>,
    pub see_also: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_sections: Vec<ArticleSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiMeta {
    pub title: String,
    pub slug: WikiSlug,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub related: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<LastVerified>,
    pub sources: Vec<PageSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<WikiSectionRef>,
    #[serde(default, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WikiSectionRef {
    pub topic: String,
    pub heading: String,
}

impl WikiPage {
    #[must_use]
    pub fn empty(slug: WikiSlug, title: String) -> Self {
        Self {
            meta: WikiMeta {
                title,
                slug,
                aliases: Vec::new(),
                tags: Vec::new(),
                entities: Vec::new(),
                related: Vec::new(),
                last_verified: None,
                sources: Vec::new(),
                sections: Vec::new(),
                extra: Extra::default(),
            },
            preamble: None,
            current_state: String::new(),
            article: String::new(),
            history: Vec::new(),
            citations: Vec::new(),
            see_also: Vec::new(),
            extra_sections: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct LastVerified(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("last_verified is neither an RFC 3339 timestamp nor a date: {0}")]
pub struct LastVerifiedError(String);

impl TryFrom<String> for LastVerified {
    type Error = LastVerifiedError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if DateTime::parse_from_rfc3339(&text).is_ok() || citations::iso_date(&text).is_some() {
            Ok(Self(text))
        } else {
            Err(LastVerifiedError(text))
        }
    }
}

impl From<DateTime<Utc>> for LastVerified {
    fn from(instant: DateTime<Utc>) -> Self {
        Self(instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }
}

impl From<LastVerified> for String {
    fn from(verified: LastVerified) -> Self {
        verified.0
    }
}

impl AsRef<str> for LastVerified {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiHistoryEntry {
    #[schemars(with = "String")]
    pub date: NaiveDate,
    pub title: String,
    pub body: String,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum WikiSourceKind {
    Summary,
    Message,
    Conversation,
    Task,
    Leaf,
    ExtractWiki,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageSource {
    pub kind: WikiSourceKind,
    pub ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageCitation {
    pub summary_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CitationKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::EnumIter,
    strum::Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum CitationKind {
    Leaf,
    Branch,
    Root,
    Condensed,
}

#[derive(Debug, thiserror::Error)]
pub enum WikiPageError {
    #[error("invalid wiki page: {0}")]
    Invalid(PageDefect),
    #[error("{0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("wiki frontmatter could not be written: {0}")]
    Emit(#[from] yaml::EmitError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum PageDefect {
    #[strum(serialize = "missing YAML frontmatter")]
    MissingFrontmatter,
    #[strum(serialize = "frontmatter not a map")]
    FrontmatterNotMap,
    #[strum(serialize = "title required")]
    TitleRequired,
    #[strum(serialize = "slug required")]
    SlugRequired,
}

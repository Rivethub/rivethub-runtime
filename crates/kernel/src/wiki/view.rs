use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use values::SessionId;

use super::page::{WikiHistoryEntry, WikiSourceKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiTopicList {
    pub topics: Vec<WikiTopic>,
    pub total: u64,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiTopic {
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    #[serde_as(as = "values::Millis")]
    pub updated_at: DateTime<Utc>,
    pub excerpt: String,
    #[serde(default)]
    pub topic_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiGapsResponse {
    pub red_links: Vec<WikiRedLink>,
    pub stalest: Vec<WikiTopic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiRedLink {
    pub entity: String,
    pub referenced_by: Vec<String>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPageResponse {
    pub slug: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub current_state: String,
    pub article: Option<String>,
    pub see_also: Option<Vec<String>>,
    pub history: Vec<WikiHistoryEntry>,
    pub citations: Vec<WikiCitation>,
    pub markdown: String,
    pub sources: Vec<WikiSource>,
    pub git_sha: Option<String>,
    #[serde(default)]
    #[serde_as(as = "Option<values::Millis>")]
    pub last_verified: Option<DateTime<Utc>>,
    #[serde_as(as = "values::Millis")]
    pub updated_at: DateTime<Utc>,
    pub related: Option<Vec<String>>,
    #[serde(default)]
    pub topic_id: Option<String>,
}

impl WikiPageResponse {
    #[must_use]
    pub fn section_view(mut self, topic: &str, heading: &str) -> Self {
        let section = section_of(&self.markdown, heading).unwrap_or_default();
        let title = format!("# {}", self.title);
        self.markdown = [title.as_str(), self.current_state.trim(), section.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n";
        self.citations
            .retain(|citation| citation.topic_id.as_deref() == Some(topic));
        self.aliases.clear();
        self.entities.clear();
        self.sources.clear();
        self
    }
}

fn section_of<'a>(markdown: &'a str, heading: &str) -> Option<&'a str> {
    let wanted = format!("## {heading}");
    let mut start = None;
    let mut end = markdown.len();
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        if line.trim_end() == wanted {
            start = Some(offset);
            end = markdown.len();
        } else if start.is_some() && end == markdown.len() && line.starts_with("## ") {
            end = offset;
        }
        offset += line.len();
    }
    markdown.get(start?..end)
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiCitation {
    pub summary_id: String,
    #[serde(default)]
    #[serde_as(as = "Option<values::Millis>")]
    pub date: Option<DateTime<Utc>>,
    pub kind: Option<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub topic_id: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiSource {
    pub kind: WikiSourceKind,
    pub ids: Vec<String>,
    pub conversation_id: Option<String>,
    pub span: Option<WikiSourceSpan>,
}

#[serde_as]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiSourceSpan {
    #[serde_as(as = "values::Millis")]
    pub earliest: DateTime<Utc>,
    #[serde_as(as = "values::Millis")]
    pub latest: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation(summary: &str, topic: Option<&str>) -> WikiCitation {
        WikiCitation {
            summary_id: summary.to_owned(),
            date: None,
            kind: None,
            note: None,
            conversation_id: None,
            session_id: None,
            title: None,
            channel: None,
            topic_id: topic.map(str::to_owned),
        }
    }

    #[test]
    fn a_subtopic_view_keeps_its_own_section_and_citations() {
        let page = WikiPageResponse {
            slug: "home".to_owned(),
            title: "Home".to_owned(),
            aliases: vec!["house".to_owned()],
            tags: vec!["place".to_owned()],
            entities: vec!["Kitchen".to_owned()],
            current_state: "The house.".to_owned(),
            article: Some("## Article\n\nRooms.".to_owned()),
            see_also: None,
            history: Vec::new(),
            citations: vec![citation("a", Some("garden")), citation("b", Some("attic"))],
            markdown: "# Home\n\n## Garden\n\nOld text.\n\n## Attic\n\nDusty.\n\n## Garden\n\nTomatoes.\n# Embedded\n\nNotes.\n## History\n\nLong.\n".to_owned(),
            sources: Vec::new(),
            git_sha: None,
            last_verified: None,
            updated_at: DateTime::<Utc>::UNIX_EPOCH,
            related: None,
            topic_id: Some("home".to_owned()),
        };
        let garden = page.clone().section_view("garden", "Garden");
        assert_eq!(
            garden.markdown,
            "# Home\n\nThe house.\n\n## Garden\n\nTomatoes.\n# Embedded\n\nNotes.\n"
        );
        assert_eq!(garden.citations, vec![citation("a", Some("garden"))]);
        assert!(garden.aliases.is_empty() && garden.entities.is_empty());
        assert_eq!(garden.article, page.article);
        assert_eq!(
            page.section_view("cellar", "Cellar").markdown,
            "# Home\n\nThe house.\n"
        );
    }
}

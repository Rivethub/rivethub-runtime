use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;

use super::DECISIONS_HEADING;
use super::DecisionStore;
use super::PageEdit;
use super::Slug;
use super::SyncTopicPage;
use super::Topic;
use super::TopicError;
use super::TopicId;
use super::TopicKey;
use super::TopicKind;
use super::TopicStore;
use super::TopicTree;
use super::page_owner;
use super::render_decisions;
use crate::BoxError;
use crate::memory::MemoryStore;
use crate::memory::MemoryStoreError;
use crate::wiki::WikiLookup;
use crate::wiki::builder::WikiStoreError;
use crate::wiki::builder::WikiWriteError;
use crate::wiki::page::FoldMark;
use crate::wiki::page::WikiSlug;
use crate::wiki::page::bound::rebase;
use crate::wiki::page::lead::steady_lead;
use crate::wiki::page::normalize_slug;

const SLUG_ATTEMPTS: u32 = 1000;
const UNNAMED_PAGE: &str = "topic";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSection {
    pub topic: Option<TopicId>,
    pub heading: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicPage {
    pub slug: WikiSlug,
    pub title: String,
    pub summary: String,
    pub article: String,
    pub sections: Vec<PageSection>,
}

impl TopicPage {
    #[must_use]
    pub fn empty(slug: WikiSlug, title: &str) -> Self {
        Self {
            slug,
            title: title.to_owned(),
            summary: String::new(),
            article: String::new(),
            sections: Vec::new(),
        }
    }

    fn text(&self) -> String {
        let mut text = String::new();
        append(&mut text, &self.summary);
        append(&mut text, &self.article);
        text
    }

    pub(crate) fn add_to_section(&mut self, topic: TopicId, heading: &str, body: &str) {
        if let Some(section) = self
            .sections
            .iter_mut()
            .find(|section| section.topic == Some(topic))
        {
            prepend(&mut section.body, body);
            return;
        }
        let mut section = PageSection {
            topic: Some(topic),
            heading: heading.to_owned(),
            body: String::new(),
        };
        prepend(&mut section.body, body);
        self.sections.push(section);
    }

    pub(crate) fn merge_section(&mut self, topic: TopicId, heading: &str, body: &str, base: &str) {
        if let Some(section) = self
            .sections
            .iter_mut()
            .find(|section| section.topic == Some(topic))
        {
            let steady = steady_lead(&section.body, body);
            section.body = rebase(base, &steady, &section.body);
            return;
        }
        self.sections.push(PageSection {
            topic: Some(topic),
            heading: heading.to_owned(),
            body: body.trim().to_owned(),
        });
    }

    pub(crate) fn add_note(&mut self, heading: &str, body: &str) {
        if let Some(section) = self.sections.iter_mut().find(|section| {
            section.topic.is_none() && section.heading.eq_ignore_ascii_case(heading)
        }) {
            prepend(&mut section.body, body);
            return;
        }
        let mut section = PageSection {
            topic: None,
            heading: heading.to_owned(),
            body: String::new(),
        };
        prepend(&mut section.body, body);
        self.sections.push(section);
    }
}

pub type PageChange<'change> = dyn for<'page> Fn(&'page mut TopicPage) + Send + Sync + 'change;

#[async_trait]
pub trait TopicPageStore: Send + Sync {
    async fn topic_page(&self, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError>;

    async fn page_taken(&self, slug: &WikiSlug) -> Result<bool, WikiStoreError>;

    async fn save_topic_page(
        &self,
        topic: TopicId,
        page: &TopicPage,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError>;

    async fn edit_page(
        &self,
        slug: &WikiSlug,
        edit: &PageEdit,
        now: DateTime<Utc>,
    ) -> Result<bool, WikiStoreError>;

    async fn change_page(
        &self,
        root: TopicId,
        slug: &WikiSlug,
        change: &PageChange<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError>;

    async fn redirect_page(
        &self,
        from: &WikiSlug,
        to: &WikiSlug,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError>;

    async fn fold_page(
        &self,
        child: &WikiSlug,
        into: &WikiSlug,
        mark: FoldMark,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError>;

    async fn remove_page(&self, slug: &WikiSlug) -> Result<(), WikiStoreError>;
}

#[derive(Debug, Error)]
pub enum TopicPageError {
    #[error(transparent)]
    Topic(#[from] TopicError),
    #[error(transparent)]
    Wiki(#[from] WikiStoreError),
    #[error(transparent)]
    Memory(#[from] MemoryStoreError),
    #[error("the parent session's placement could not be read: {0}")]
    ParentPlacement(#[source] BoxError),
    #[error("sync-topic-page payload: {0}")]
    Payload(#[source] BoxError),
}

pub async fn bound_page(
    topics: &dyn TopicStore,
    memory: &dyn MemoryStore,
    topic: &Topic,
) -> Result<Option<crate::wiki::WikiPageResponse>, TopicPageError> {
    let Some(owner) = page_owner(topics, topic).await? else {
        return Ok(None);
    };
    let Some(slug) = topics.page_slug(owner.id).await? else {
        return Ok(None);
    };
    Ok(match memory.wiki_page(&slug).await? {
        WikiLookup::Page(page) => Some(*page),
        WikiLookup::Redirect(_) | WikiLookup::Missing => None,
    })
}

pub(crate) async fn root_page(
    topics: &dyn TopicStore,
    pages: &dyn TopicPageStore,
    root: &Topic,
    now: DateTime<Utc>,
) -> Result<WikiSlug, TopicPageError> {
    PageSync { topics, pages, now }.ensure(root).await
}

pub async fn sync_topic_page(
    topics: &dyn TopicStore,
    pages: &dyn TopicPageStore,
    decisions: &dyn DecisionStore,
    job: &SyncTopicPage,
    now: DateTime<Utc>,
) -> Result<(), TopicPageError> {
    let sync = PageSync { topics, pages, now };
    if let Some(id) = job.topic_id {
        return sync.topic(id, &job.fold, decisions).await;
    }
    for listing in topics.list().await? {
        let topic = listing.topic;
        let owns_page = match topic.kind {
            TopicKind::Type => false,
            TopicKind::Entity => true,
            TopicKind::Work => {
                topic.parent.is_none() || topics.page_slug(topic.id).await?.is_some()
            }
        };
        if owns_page {
            sync.topic(topic.id, &[], decisions).await?;
        }
    }
    Ok(())
}

enum Landing {
    Article,
    Section(TopicId),
    Elsewhere(Box<Topic>),
    Plain,
}

struct PageSync<'a> {
    topics: &'a dyn TopicStore,
    pages: &'a dyn TopicPageStore,
    now: DateTime<Utc>,
}

impl PageSync<'_> {
    async fn topic(
        &self,
        id: TopicId,
        fold: &[WikiSlug],
        decisions: &dyn DecisionStore,
    ) -> Result<(), TopicPageError> {
        let Some(topic) = self.topics.find(&TopicKey::Id(id)).await? else {
            return Ok(());
        };
        let Some(root) = page_owner(self.topics, &topic).await? else {
            return Ok(());
        };
        let home = self.ensure(&root).await?;
        if root.id != topic.id
            && let Some(own) = self.topics.page_slug(topic.id).await?
            && own != home
        {
            self.absorb(&root, &topic, &own, &home).await?;
        }
        for child in fold.iter().filter(|child| **child != home) {
            self.pages
                .fold_page(child, &home, FoldMark::Merged, self.now)
                .await?;
        }
        self.reconcile(&root, &home, decisions).await
    }

    async fn ensure(&self, root: &Topic) -> Result<WikiSlug, TopicPageError> {
        if let Some(slug) = self.topics.page_slug(root.id).await? {
            return Ok(slug);
        }
        let slug = self.free_slug(&root.slug).await?;
        let page = TopicPage::empty(slug.clone(), root.name.as_str());
        self.pages.save_topic_page(root.id, &page, self.now).await?;
        Ok(slug)
    }

    async fn free_slug(&self, topic: &Slug) -> Result<WikiSlug, TopicPageError> {
        let base = page_slug_text(topic);
        for attempt in 1..=SLUG_ATTEMPTS {
            let text = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let slug = wiki_slug(text)?;
            if !self.pages.page_taken(&slug).await? {
                return Ok(slug);
            }
        }
        Err(WikiStoreError::SlugConflict(base).into())
    }

    async fn read(&self, slug: &WikiSlug) -> Result<TopicPage, TopicPageError> {
        self.pages
            .topic_page(slug)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()).into())
    }

    async fn save(
        &self,
        owner: TopicId,
        before: &TopicPage,
        after: &TopicPage,
    ) -> Result<(), TopicPageError> {
        if before != after {
            self.pages.save_topic_page(owner, after, self.now).await?;
        }
        Ok(())
    }

    async fn absorb(
        &self,
        root: &Topic,
        topic: &Topic,
        own: &WikiSlug,
        home: &WikiSlug,
    ) -> Result<(), TopicPageError> {
        let Some(child) = self.pages.topic_page(own).await? else {
            return Ok(());
        };
        let text = child.text();
        if !text.is_empty() {
            let before = self.read(home).await?;
            let mut after = before.clone();
            after.add_to_section(topic.id, topic.name.as_str(), &text);
            self.save(root.id, &before, &after).await?;
            let mut emptied = child.clone();
            emptied.summary.clear();
            emptied.article.clear();
            self.save(topic.id, &child, &emptied).await?;
        }
        self.pages
            .fold_page(own, home, FoldMark::Section, self.now)
            .await?;
        Ok(())
    }

    async fn reconcile(
        &self,
        root: &Topic,
        home: &WikiSlug,
        decisions: &dyn DecisionStore,
    ) -> Result<(), TopicPageError> {
        let before = self.read(home).await?;
        let subtopics = self.subtopics(root.id).await?;
        let decided = decided(root, &subtopics, decisions).await?;
        let mut owned: Vec<PageSection> = Vec::new();
        let mut plain: Vec<PageSection> = Vec::new();
        let mut into_article: Vec<String> = Vec::new();
        let mut into_sections: Vec<(TopicId, String)> = Vec::new();
        let mut moved: Vec<(Topic, String)> = Vec::new();
        for section in &before.sections {
            match section.topic {
                None if section.heading == DECISIONS_HEADING => {}
                None => plain.push(section.clone()),
                Some(id) if subtopics.iter().any(|sub| sub.id == id) => {
                    match owned.iter_mut().find(|kept| kept.topic == Some(id)) {
                        Some(kept) => append(&mut kept.body, &section.body),
                        None => owned.push(section.clone()),
                    }
                }
                Some(_) => match self.landing(root, &subtopics, section).await? {
                    Landing::Article => into_article.push(section.body.clone()),
                    Landing::Section(id) => into_sections.push((id, section.body.clone())),
                    Landing::Elsewhere(target) => moved.push((*target, section.body.clone())),
                    Landing::Plain => plain.push(PageSection {
                        topic: None,
                        ..section.clone()
                    }),
                },
            }
        }
        let mut after = before.clone();
        root.name.as_str().clone_into(&mut after.title);
        after.sections = arranged(&subtopics, owned, &mut plain, &into_sections);
        after.sections.extend(plain);
        if !decided.is_empty() {
            after.sections.insert(
                0,
                PageSection {
                    topic: None,
                    heading: DECISIONS_HEADING.to_owned(),
                    body: decided,
                },
            );
        }
        for body in &into_article {
            append(&mut after.article, body);
        }
        for (target, body) in &moved {
            self.hand_off(target, body).await?;
        }
        self.save(root.id, &before, &after).await?;
        let topic_slug = wiki_slug(page_slug_text(&root.slug))?;
        if topic_slug != *home {
            self.pages
                .redirect_page(&topic_slug, home, self.now)
                .await?;
        }
        Ok(())
    }

    async fn subtopics(&self, root: TopicId) -> Result<Vec<Topic>, TopicError> {
        let topics: Vec<Topic> = self
            .topics
            .list()
            .await?
            .into_iter()
            .map(|listing| listing.topic)
            .collect();
        let tree = TopicTree::of(&topics);
        let mut subtopics: Vec<Topic> = topics
            .into_iter()
            .filter(|topic| topic.id != root && tree.within(topic.id, root))
            .collect();
        subtopics.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.slug.cmp(&right.slug))
        });
        Ok(subtopics)
    }

    async fn landing(
        &self,
        root: &Topic,
        subtopics: &[Topic],
        section: &PageSection,
    ) -> Result<Landing, TopicError> {
        let found = match section.topic {
            Some(id) => self.topics.find(&TopicKey::Id(id)).await?,
            None => None,
        };
        let target = match found {
            Some(topic) => Some(topic),
            None => match Slug::of_heading(&section.heading) {
                Some(slug) => {
                    self.topics
                        .find(&TopicKey::Slug(TopicKind::Work, slug))
                        .await?
                }
                None => None,
            },
        };
        Ok(match target {
            None => Landing::Plain,
            Some(topic) if topic.id == root.id => Landing::Article,
            Some(topic) if subtopics.iter().any(|sub| sub.id == topic.id) => {
                Landing::Section(topic.id)
            }
            Some(topic) => Landing::Elsewhere(Box::new(topic)),
        })
    }

    async fn hand_off(&self, target: &Topic, body: &str) -> Result<(), TopicPageError> {
        if body.trim().is_empty() {
            return Ok(());
        }
        let Some(owner) = page_owner(self.topics, target).await? else {
            return Ok(());
        };
        let slug = self.ensure(&owner).await?;
        let before = self.read(&slug).await?;
        let mut after = before.clone();
        if owner.id == target.id {
            append(&mut after.article, body);
        } else {
            after.add_to_section(target.id, target.name.as_str(), body);
        }
        self.save(owner.id, &before, &after).await
    }
}

async fn decided(
    root: &Topic,
    subtopics: &[Topic],
    decisions: &dyn DecisionStore,
) -> Result<String, TopicError> {
    let own = decisions.decisions(root.id).await?;
    let mut theirs = Vec::with_capacity(subtopics.len());
    for sub in subtopics {
        theirs.push((sub, decisions.decisions(sub.id).await?));
    }
    Ok(render_decisions(&own, &theirs))
}

fn arranged(
    subtopics: &[Topic],
    mut owned: Vec<PageSection>,
    plain: &mut Vec<PageSection>,
    additions: &[(TopicId, String)],
) -> Vec<PageSection> {
    subtopics
        .iter()
        .map(|sub| {
            let mut section = match owned.iter().position(|kept| kept.topic == Some(sub.id)) {
                Some(index) => owned.remove(index),
                None => match plain
                    .iter()
                    .position(|loose| loose.heading.eq_ignore_ascii_case(sub.name.as_str()))
                {
                    Some(index) => plain.remove(index),
                    None => PageSection {
                        topic: None,
                        heading: String::new(),
                        body: String::new(),
                    },
                },
            };
            section.topic = Some(sub.id);
            sub.name.as_str().clone_into(&mut section.heading);
            for (_, body) in additions.iter().filter(|(id, _)| *id == sub.id) {
                append(&mut section.body, body);
            }
            section
        })
        .collect()
}

fn prepend(target: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() || target.contains(text) {
        return;
    }
    *target = if target.trim().is_empty() {
        text.to_owned()
    } else {
        format!("{text}\n\n{}", target.trim_start())
    };
}

fn append(target: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() || target.contains(text) {
        return;
    }
    *target = if target.trim().is_empty() {
        text.to_owned()
    } else {
        format!("{}\n\n{text}", target.trim_end())
    };
}

fn page_slug_text(topic: &Slug) -> String {
    let slug = normalize_slug(topic.as_str());
    if slug.is_empty() {
        UNNAMED_PAGE.to_owned()
    } else {
        slug
    }
}

fn wiki_slug(text: String) -> Result<WikiSlug, WikiStoreError> {
    WikiSlug::try_from(text).map_err(|error| WikiStoreError::Page(WikiWriteError::Slug(error)))
}

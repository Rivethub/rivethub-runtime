use std::str::FromStr;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;
use values::SessionId;

use super::builder::DeferredWiki;
use super::builder::ExtractWiki;
use super::builder::HeldEntity;
use super::builder::WikiBuilderStore;
use super::builder::WikiRouting;
use super::builder::WikiSummary;
use super::entity::Catalog;
use super::entity::Filed;
use super::merge::MergedPage;
use super::merge::PageMerger;
use super::origin::NoteOrigin;
use super::page::WikiPatch;
use super::page::WikiSlug;
use super::page::bound::MergeBase;
use super::page::demote_h2_headings;
use crate::filing::Filable;
use crate::filing::FilingStore;
use crate::filing::Target;
use crate::memory::ConversationHead;
use crate::topics::PageSection;
use crate::topics::PlacementSource;
use crate::topics::Topic;
use crate::topics::TopicId;
use crate::topics::TopicKey;
use crate::topics::TopicPage;
use crate::topics::TopicPageError;
use crate::topics::TopicPageStore;
use crate::topics::TopicStore;
use crate::topics::page_owner;
use crate::topics::root_page;
use crate::transcript::SessionLister;
use crate::transcript::TranscriptReaderError;
use crate::transcript::linked_parent;
use crate::transcript::session_parent;

mod current;

const DEFERRALS_PER_SWEEP: u32 = 50;
const LINEAGE_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicTarget {
    #[serde(skip)]
    pub topic: TopicId,
    #[serde(skip)]
    pub root: TopicId,
    pub page_slug: WikiSlug,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<CurrentText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentText {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub article: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl TopicTarget {
    #[must_use]
    pub fn route_key(&self) -> String {
        match self.section {
            Some(_) => format!("{}#{}", self.page_slug.as_ref(), self.topic),
            None => self.page_slug.as_ref().to_owned(),
        }
    }

    fn claims(&self, patch: &WikiPatch) -> bool {
        patch.slug == self.page_slug || patch.aliases.contains(&self.page_slug)
    }

    fn page_patch(&self, mut patch: WikiPatch) -> WikiPatch {
        patch.slug = self.page_slug.clone();
        patch.aliases.retain(|alias| *alias != self.page_slug);
        self.name.clone_into(&mut patch.title);
        patch
    }
}

#[expect(
    missing_debug_implementations,
    reason = "kernel ports are trait objects"
)]
#[derive(Clone, Copy)]
pub struct TopicPorts<'a> {
    pub topics: &'a dyn TopicStore,
    pub pages: &'a dyn TopicPageStore,
    pub filing: &'a dyn FilingStore,
    pub lineage: &'a dyn SessionLister,
}

#[expect(
    missing_debug_implementations,
    reason = "kernel ports are trait objects"
)]
#[derive(Clone, Copy)]
pub struct WikiRouter<'a> {
    pub wiki: &'a dyn WikiBuilderStore,
    pub topics: TopicPorts<'a>,
    pub merger: Option<&'a dyn PageMerger>,
    pub now: DateTime<Utc>,
}

#[derive(Default)]
struct Routed {
    pages: Vec<WikiPatch>,
    written: Vec<WikiSlug>,
    cited: Vec<WikiSlug>,
    earlier: Vec<(Uuid, WikiSlug)>,
    held: Vec<HeldEntity>,
    notes: Vec<WikiPatch>,
    merged: Vec<MergedPage>,
}

impl WikiRouter<'_> {
    pub async fn target(
        &self,
        summary: &WikiSummary,
    ) -> Result<Option<TopicTarget>, TopicPageError> {
        match self.route(summary).await? {
            Some(target) => self.current(summary, target).await.map(Some),
            None => Ok(None),
        }
    }

    pub async fn route(
        &self,
        summary: &WikiSummary,
    ) -> Result<Option<TopicTarget>, TopicPageError> {
        match self.topics.waits_on(summary).await? {
            Some(waits) => self.topics.filed_target(&waits, self.now).await,
            None => Ok(None),
        }
    }

    pub async fn current(
        &self,
        summary: &WikiSummary,
        target: TopicTarget,
    ) -> Result<TopicTarget, TopicPageError> {
        NoteOrigin::run(summary.id, Box::pin(self.with_current(target))).await
    }

    pub async fn type_paths(&self) -> Result<Vec<String>, TopicPageError> {
        Ok(Catalog::load(self.topics.topics).await?.type_paths())
    }

    pub async fn apply(
        &self,
        job: &ExtractWiki,
        summary: &WikiSummary,
        target: Option<&TopicTarget>,
        patches: Vec<WikiPatch>,
    ) -> Result<Vec<String>, TopicPageError> {
        self.apply_with(job, summary, Vec::new(), target, patches)
            .await
    }

    pub async fn apply_with(
        &self,
        job: &ExtractWiki,
        summary: &WikiSummary,
        also: Vec<WikiSummary>,
        target: Option<&TopicTarget>,
        patches: Vec<WikiPatch>,
    ) -> Result<Vec<String>, TopicPageError> {
        NoteOrigin::run(
            summary.id,
            Box::pin(self.apply_notes(job, summary, also, target, patches)),
        )
        .await
    }

    async fn apply_notes(
        &self,
        job: &ExtractWiki,
        summary: &WikiSummary,
        also: Vec<WikiSummary>,
        target: Option<&TopicTarget>,
        patches: Vec<WikiPatch>,
    ) -> Result<Vec<String>, TopicPageError> {
        let mut routed = Routed {
            cited: target
                .map(|topic| topic.page_slug.clone())
                .into_iter()
                .collect(),
            ..Routed::default()
        };
        let mut entities = None;
        for patch in patches {
            if let Some(topic) = target.filter(|topic| topic.claims(&patch)) {
                self.route_to_topic(topic, patch, &mut routed).await?;
            } else if patch.entity_type.is_some() {
                let known = match entities.take() {
                    Some(known) => known,
                    None => Catalog::load(self.topics.topics).await?,
                };
                entities = Some(
                    self.route_to_entity(known, summary, patch, &mut routed)
                        .await?,
                );
            } else {
                if let Some(topic) = target {
                    self.write_note(topic, &patch).await?;
                    routed.written.push(topic.page_slug.clone());
                }
                routed.notes.push(patch);
            }
        }
        self.finish(job, summary, also, target, routed).await
    }

    pub async fn apply_deferred(&self) -> Result<Vec<String>, TopicPageError> {
        let mut touched = Vec::new();
        for held in self.wiki.unsettled_notes(DEFERRALS_PER_SWEEP).await? {
            let summary = held.summary_id;
            touched.extend(NoteOrigin::run(summary, Box::pin(self.settle(held))).await?);
        }
        touched.sort();
        touched.dedup();
        Ok(touched)
    }

    async fn settle(&self, held: DeferredWiki) -> Result<Vec<String>, TopicPageError> {
        let mut touched = Vec::new();
        if let Some(old) = held.applied
            && let Some(topic) = self.topics.topics.find(&TopicKey::Id(old)).await?
            && let Some(route) = self.topics.target(&topic, self.now).await?
        {
            for patch in &held.patches {
                self.withdraw_note(&route, patch).await?;
            }
            self.wiki
                .uncite_wiki(held.summary_id, &route.page_slug)
                .await?;
            touched.push(route.page_slug.as_ref().to_owned());
        }
        let route = self.topics.filed_target(&held.waits, self.now).await?;
        if let Some(route) = &route {
            for patch in &held.patches {
                self.write_note(route, patch).await?;
            }
            self.wiki
                .cite_wiki(held.summary_id, &route.page_slug, self.now)
                .await?;
            touched.push(route.page_slug.as_ref().to_owned());
        }
        self.wiki
            .defer_wiki(DeferredWiki {
                applied: route.map(|route| route.topic),
                ..held
            })
            .await?;
        Ok(touched)
    }

    async fn route_to_topic(
        &self,
        topic: &TopicTarget,
        patch: WikiPatch,
        routed: &mut Routed,
    ) -> Result<(), TopicPageError> {
        if topic.section.is_none() {
            if let Some(current) = &topic.current {
                routed.merged.push(MergedPage {
                    slug: topic.page_slug.clone(),
                    base: MergeBase {
                        summary: current.summary.clone(),
                        article: current.article.clone().unwrap_or_default(),
                    },
                });
            }
            routed.pages.push(topic.page_patch(patch));
            return Ok(());
        }
        self.write_section(topic, &patch).await?;
        routed.written.push(topic.page_slug.clone());
        Ok(())
    }

    async fn route_to_entity(
        &self,
        mut known: Catalog,
        summary: &WikiSummary,
        patch: WikiPatch,
        routed: &mut Routed,
    ) -> Result<Catalog, TopicPageError> {
        let filing = known
            .file(self.topics.topics, self.wiki, summary, patch, self.now)
            .await?;
        if let Some(Filed {
            entity,
            patches,
            earlier,
            held,
        }) = filing
            && let Some(page) = self.topics.target(&entity, self.now).await?
        {
            routed.held.extend(held);
            routed
                .earlier
                .extend(earlier.into_iter().map(|id| (id, page.page_slug.clone())));
            let patches = patches
                .into_iter()
                .map(|patch| page.page_patch(patch))
                .collect();
            let patches = self
                .merge_entity(&page, patches, &mut routed.merged)
                .await?;
            routed.pages.extend(patches);
        }
        Ok(known)
    }

    async fn finish(
        &self,
        job: &ExtractWiki,
        summary: &WikiSummary,
        also: Vec<WikiSummary>,
        target: Option<&TopicTarget>,
        mut routed: Routed,
    ) -> Result<Vec<String>, TopicPageError> {
        if !routed.notes.is_empty()
            && let Some(waits) = self.topics.waits_on(summary).await?
        {
            self.wiki
                .defer_wiki(DeferredWiki {
                    summary_id: summary.id,
                    waits,
                    applied: target.map(|topic| topic.topic),
                    patches: routed.notes,
                    at: self.now,
                })
                .await?;
        }
        routed.cited.extend(routed.written.iter().cloned());
        routed
            .cited
            .sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
        routed.cited.dedup();
        let routing = WikiRouting {
            topic_page: target.map(|topic| topic.page_slug.clone()),
            cited: routed.cited,
            merged: routed.merged,
            also,
        };
        let mut touched = self
            .wiki
            .apply_wiki(job, routed.pages, &routing, self.now)
            .await?;
        for (earlier, page) in &routed.earlier {
            self.wiki.cite_wiki(*earlier, page, self.now).await?;
        }
        if !routed.held.is_empty() {
            self.wiki.release_entity(&routed.held).await?;
        }
        touched.extend(routed.written.into_iter().map(String::from));
        touched.sort();
        touched.dedup();
        Ok(touched)
    }

    async fn write_section(
        &self,
        route: &TopicTarget,
        patch: &WikiPatch,
    ) -> Result<(), TopicPageError> {
        let Some(heading) = &route.section else {
            return Ok(());
        };
        let text = section_text(patch);
        let base = route
            .current
            .as_ref()
            .and_then(|current| current.section.clone());
        let topic = route.topic;
        self.edit_page(route, move |page| match &base {
            Some(base) => page.merge_section(topic, heading, &text, base),
            None => page.add_to_section(topic, heading, &text),
        })
        .await
    }

    async fn write_note(
        &self,
        route: &TopicTarget,
        patch: &WikiPatch,
    ) -> Result<(), TopicPageError> {
        let (topic, heading, body) = note_parts(route, patch);
        self.edit_page(route, |page| match topic {
            Some(topic) => page.add_to_section(topic, &heading, &body),
            None => page.add_note(&heading, &body),
        })
        .await
    }

    async fn withdraw_note(
        &self,
        route: &TopicTarget,
        patch: &WikiPatch,
    ) -> Result<(), TopicPageError> {
        let (topic, heading, body) = note_parts(route, patch);
        self.edit_page(route, |page| withdraw(page, topic, &heading, &body))
            .await
    }

    async fn edit_page(
        &self,
        route: &TopicTarget,
        change: impl Fn(&mut TopicPage) + Send + Sync,
    ) -> Result<(), TopicPageError> {
        self.topics
            .pages
            .change_page(route.root, &route.page_slug, &change, self.now)
            .await?;
        Ok(())
    }
}

impl TopicPorts<'_> {
    async fn waits_on(&self, summary: &WikiSummary) -> Result<Option<Target>, TopicPageError> {
        let filed = match summary.conversation_id {
            Some(conversation) => match self.filing.subject(conversation).await? {
                Some(subject) => self.followed(&subject.head, conversation).await?,
                None => None,
            },
            None => session(summary).map(Target::Session),
        };
        let Some(Target::Session(mut session)) = filed else {
            return Ok(filed);
        };
        for _ in 0..LINEAGE_DEPTH {
            if self.topics.placement(&session).await?.is_some() {
                break;
            }
            match self
                .parent(&session)
                .await
                .map_err(|error| TopicPageError::ParentPlacement(error.into()))?
            {
                Some(parent) => session = parent,
                None => break,
            }
        }
        Ok(Some(Target::Session(session)))
    }

    async fn followed(
        &self,
        head: &ConversationHead,
        conversation: Uuid,
    ) -> Result<Option<Target>, TopicPageError> {
        let filable = Filable::of(head);
        let Filable::Task(task) = filable else {
            return Ok(filable.target(conversation));
        };
        let own = self.filing.conversation_placement(conversation).await?;
        if own.is_some_and(|placed| placed.by == PlacementSource::You) {
            return Ok(Some(Target::Conversation(conversation)));
        }
        Ok(match self.filing.task_parent(task, conversation).await? {
            Some(parent) => Filable::of(&parent).target(parent.id),
            None => Some(Target::Conversation(conversation)),
        })
    }

    async fn filed_target(
        &self,
        waits: &Target,
        now: DateTime<Utc>,
    ) -> Result<Option<TopicTarget>, TopicPageError> {
        let topic = match waits {
            Target::Session(session) => self
                .topics
                .placement(session)
                .await?
                .map(|placement| placement.topic),
            Target::Conversation(conversation) => self
                .filing
                .conversation_placement(*conversation)
                .await?
                .map(|placement| placement.topic),
        };
        let Some(topic) = topic else {
            return Ok(None);
        };
        match self.topics.find(&TopicKey::Id(topic)).await? {
            Some(topic) => self.target(&topic, now).await,
            None => Ok(None),
        }
    }

    async fn parent(
        &self,
        session: &SessionId,
    ) -> Result<Option<SessionId>, TranscriptReaderError> {
        Ok(session_parent(self.lineage, session)
            .await?
            .and_then(|parent| linked_parent(session.harness(), &parent)))
    }

    async fn target(
        &self,
        topic: &Topic,
        now: DateTime<Utc>,
    ) -> Result<Option<TopicTarget>, TopicPageError> {
        let Some(owner) = page_owner(self.topics, topic).await? else {
            return Ok(None);
        };
        let page_slug = root_page(self.topics, self.pages, &owner, now).await?;
        Ok(Some(TopicTarget {
            topic: topic.id,
            root: owner.id,
            page_slug,
            name: owner.name.as_str().to_owned(),
            section: (owner.id != topic.id).then(|| topic.name.as_str().to_owned()),
            current: None,
        }))
    }
}

fn session(summary: &WikiSummary) -> Option<SessionId> {
    let key = summary.session_key.as_deref()?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a conversation whose session key is not a canonical session id has no topic placement"
        )
    )]
    let Ok(session) = SessionId::from_str(key) else {
        return None;
    };
    Some(session)
}

fn note_parts(route: &TopicTarget, patch: &WikiPatch) -> (Option<TopicId>, String, String) {
    let text = section_text(patch);
    match &route.section {
        Some(heading) => (
            Some(route.topic),
            heading.clone(),
            format!("### {}\n\n{text}", patch.title.trim()),
        ),
        None => (None, patch.title.trim().to_owned(), text),
    }
}

fn withdraw(page: &mut TopicPage, topic: Option<TopicId>, heading: &str, body: &str) {
    let body = body.trim();
    let holds = |section: &PageSection| match topic {
        Some(topic) => section.topic == Some(topic),
        None => section.topic.is_none() && section.heading.eq_ignore_ascii_case(heading),
    };
    for section in page.sections.iter_mut().filter(|section| holds(section)) {
        if let Some(at) = section.body.find(body) {
            let (before, rest) = section.body.split_at(at);
            let before = before.trim_end();
            let after = rest.get(body.len()..).unwrap_or_default().trim_start();
            section.body = match (before.is_empty(), after.is_empty()) {
                (true, _) => after.to_owned(),
                (_, true) => before.to_owned(),
                (false, false) => format!("{before}\n\n{after}"),
            };
        }
    }
    page.sections
        .retain(|section| !(holds(section) && section.body.trim().is_empty()));
}

fn section_text(patch: &WikiPatch) -> String {
    let summary = demote_h2_headings(patch.summary.trim());
    match patch
        .article
        .as_deref()
        .map(str::trim)
        .filter(|article| !article.is_empty())
    {
        Some(article) => format!("{summary}\n\n{}", demote_h2_headings(article)),
        None => summary,
    }
}

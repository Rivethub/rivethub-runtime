use kernel::memory::MemoryStore;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::topics::Change;
use kernel::topics::Decision;
use kernel::topics::DecisionStore;
use kernel::topics::DecisionText;
use kernel::topics::NewTopic;
use kernel::topics::PageEdit;
use kernel::topics::PageSection;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Slug;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::topics::edit_topic_page;
use kernel::topics::sync_topic_page;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiPageResponse;
use kernel::wiki::page::WikiSlug;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;
use crate::topics::session;

#[expect(
    missing_debug_implementations,
    reason = "the stores are kernel ports without Debug"
)]
#[derive(Clone, Copy)]
pub struct PageStores<'a> {
    pub topics: &'a dyn TopicStore,
    pub pages: &'a dyn TopicPageStore,
    pub decisions: &'a dyn DecisionStore,
    pub wiki: &'a dyn MemoryStore,
    pub queue: &'a dyn JobQueue,
}

impl PageStores<'_> {
    pub(crate) async fn settle(&self) -> Outcome {
        let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::SyncTopicPage)]);
        while let Some(job) = self.queue.claim(&selector).await? {
            let sync = SyncTopicPage::from_payload(&job.payload)?;
            sync_topic_page(
                self.topics,
                self.pages,
                self.decisions,
                &sync,
                SweepOwners::All,
                at(30),
            )
            .await?;
            self.queue.complete(job.id).await?;
        }
        Ok(())
    }

    pub(crate) async fn create(&self, topic: Topic) -> Result<Topic, Box<dyn std::error::Error>> {
        let created = self.topics.create(topic).await?;
        self.settle().await?;
        Ok(created)
    }

    async fn update(&self, topic: &Topic, changes: TopicChanges) -> Outcome {
        self.topics.update(topic.id, changes, at(10)).await?;
        self.settle().await
    }

    pub(crate) async fn slug(&self, topic: &Topic) -> Result<WikiSlug, Box<dyn std::error::Error>> {
        Ok(self
            .topics
            .page_slug(topic.id)
            .await?
            .ok_or("topic has no page")?)
    }

    pub(crate) async fn page(
        &self,
        topic: &Topic,
    ) -> Result<TopicPage, Box<dyn std::error::Error>> {
        let slug = self.slug(topic).await?;
        Ok(self
            .pages
            .topic_page(&slug)
            .await?
            .ok_or("page is missing")?)
    }

    pub(crate) async fn lookup(
        &self,
        slug: &str,
    ) -> Result<WikiLookup, Box<dyn std::error::Error>> {
        Ok(self
            .wiki
            .wiki_page(&WikiSlug::try_from(slug.to_owned())?)
            .await?)
    }

    pub(crate) async fn shown(
        &self,
        slug: &str,
    ) -> Result<WikiPageResponse, Box<dyn std::error::Error>> {
        match self.lookup(slug).await? {
            WikiLookup::Page(page) => Ok(*page),
            other @ (WikiLookup::Redirect(_) | WikiLookup::Missing) => {
                Err(format!("{slug} is not a page: {other:?}").into())
            }
        }
    }
}

fn renamed(name: &str) -> Result<TopicChanges, kernel::topics::TopicError> {
    Ok(TopicChanges {
        name: Some(TopicName::new(name)?),
        ..TopicChanges::default()
    })
}

pub async fn a_new_root_topic_gets_an_empty_bound_page(stores: PageStores<'_>) -> Outcome {
    let topic = stores.create(new_topic("Mesh Network", &[], None)?).await?;
    let slug = stores.slug(&topic).await?;
    same("page slug", slug.as_ref(), "mesh-network")?;
    let page = stores.shown("mesh-network").await?;
    same("title", page.title.as_str(), "Mesh Network")?;
    same("summary", page.current_state.as_str(), "")?;
    same("article", page.article, None)?;
    same("bound", page.topic_id, Some(topic.id.to_string()))?;
    Ok(())
}

pub async fn a_rename_keeps_the_page_slug_and_follows_the_name(stores: PageStores<'_>) -> Outcome {
    let topic = stores.create(new_topic("Mesh", &[], None)?).await?;
    stores.update(&topic, renamed("Fabric")?).await?;
    same("page slug", stores.slug(&topic).await?.as_ref(), "mesh")?;
    same(
        "title",
        stores.shown("mesh").await?.title.as_str(),
        "Fabric",
    )?;
    same(
        "new slug",
        stores.lookup("fabric").await?,
        WikiLookup::Redirect("mesh".to_owned()),
    )?;
    Ok(())
}

pub async fn a_merge_folds_the_page_and_redirects_its_slug(stores: PageStores<'_>) -> Outcome {
    let alpha = stores.create(new_topic("Alpha", &[], None)?).await?;
    let beta = stores.create(new_topic("Beta", &[], None)?).await?;
    let mut page = stores.page(&alpha).await?;
    "Alpha facts.".clone_into(&mut page.summary);
    stores.pages.save_topic_page(alpha.id, &page, at(5)).await?;
    stores.topics.merge(alpha.id, beta.id, at(10)).await?;
    stores.settle().await?;
    same(
        "folded slug",
        stores.lookup("alpha").await?,
        WikiLookup::Redirect("beta".to_owned()),
    )?;
    let merged = stores.shown("beta").await?;
    same(
        "folded text",
        merged.markdown.contains("Alpha facts."),
        true,
    )?;
    same("kept binding", stores.slug(&beta).await?.as_ref(), "beta")?;
    Ok(())
}

pub async fn sub_topics_are_sections_of_the_root_page(stores: PageStores<'_>) -> Outcome {
    let root = stores.create(new_topic("Infra", &[], None)?).await?;
    let sub = stores
        .create(new_topic("Certificates", &[], Some(root.id))?)
        .await?;
    same(
        "sub-topic page",
        stores.topics.page_slug(sub.id).await?,
        None,
    )?;
    same(
        "section",
        stores.page(&root).await?.sections,
        vec![PageSection {
            topic: Some(sub.id),
            heading: "Certificates".to_owned(),
            body: String::new(),
        }],
    )?;
    let shown = stores.shown("infra").await?.markdown;
    same("heading", shown.contains("## Certificates"), true)?;
    stores.update(&sub, renamed("Certs")?).await?;
    let mut page = stores.page(&root).await?;
    same(
        "renamed heading",
        page.sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect::<Vec<_>>(),
        vec!["Certs"],
    )?;
    for section in &mut page.sections {
        "Renewal runbook.".clone_into(&mut section.body);
    }
    stores.pages.save_topic_page(root.id, &page, at(12)).await?;
    let promoted = TopicChanges {
        parent: Change::Clear,
        ..TopicChanges::default()
    };
    stores.update(&sub, promoted).await?;
    same(
        "emptied root",
        stores.page(&root).await?.sections,
        Vec::new(),
    )?;
    let own = stores.page(&sub).await?;
    same("own page", own.slug.as_ref(), "certs")?;
    same("section text", own.article.as_str(), "Renewal runbook.")?;
    let demoted = TopicChanges {
        parent: Change::Set(root.id),
        ..TopicChanges::default()
    };
    stores.update(&sub, demoted).await?;
    same("unbound", stores.topics.page_slug(sub.id).await?, None)?;
    same(
        "fold marker",
        stores
            .shown("infra")
            .await?
            .markdown
            .contains("Merged from"),
        false,
    )?;
    same(
        "folded slug",
        stores.lookup("certs").await?,
        WikiLookup::Redirect("infra".to_owned()),
    )?;
    let sections = stores.page(&root).await?.sections;
    same(
        "section back",
        sections
            .iter()
            .map(|section| (section.topic, section.body.as_str()))
            .collect::<Vec<_>>(),
        vec![(Some(sub.id), "Renewal runbook.")],
    )?;
    Ok(())
}

pub async fn syncing_twice_changes_nothing(stores: PageStores<'_>) -> Outcome {
    let root = stores.create(new_topic("Storage", &[], None)?).await?;
    stores
        .create(new_topic("Backups", &[], Some(root.id))?)
        .await?;
    stores.update(&root, renamed("Disks")?).await?;
    let before = (
        stores.shown("storage").await?,
        stores.lookup("disks").await?,
    );
    sync_topic_page(
        stores.topics,
        stores.pages,
        stores.decisions,
        &SyncTopicPage::every_topic(),
        SweepOwners::All,
        at(40),
    )
    .await?;
    let after = (
        stores.shown("storage").await?,
        stores.lookup("disks").await?,
    );
    same("second sync", after, before)?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one topic deletion, from its page to its placed and sub-topic sessions"
)]
pub async fn a_deleted_topic_leaves_its_sessions_forming_and_its_page_unbound(
    stores: PageStores<'_>,
) -> Outcome {
    let topic = stores
        .create(new_topic("Desktop smoke", &[], None)?)
        .await?;
    let mut page = stores.page(&topic).await?;
    "Smoke notes.".clone_into(&mut page.summary);
    stores.pages.save_topic_page(topic.id, &page, at(5)).await?;
    stores.update(&topic, renamed("Smoke run")?).await?;
    let placed = session("00000000-0000-4000-8000-000000000031")?;
    stores
        .topics
        .place(Placement {
            session: placed.clone(),
            topic: topic.id,
            by: PlacementSource::You,
            at: at(11),
            owner: Some(crate::owner()?),
        })
        .await?;
    let sub = stores
        .create(new_topic("Smoke step", &[], Some(topic.id))?)
        .await?;
    let refused = stores.topics.delete(topic.id).await;
    if !matches!(refused, Err(TopicError::SubtopicsRemain { .. })) {
        return Err(format!("a topic with sub-topics must stay: {refused:?}").into());
    }
    let decided = Decision::new(
        topic.id,
        DecisionText::new("Smoke daily")?,
        "phil",
        None,
        at(12),
    );
    let sub_decided = Decision::new(
        sub.id,
        DecisionText::new("Smoke before merge")?,
        "phil",
        None,
        at(12),
    );
    stores
        .decisions
        .keep_decisions(&[decided.clone(), sub_decided.clone()])
        .await?;
    stores.topics.delete(sub.id).await?;
    same(
        "sub-topic decision",
        stores.decisions.decision(sub_decided.id).await?,
        None,
    )?;
    stores.topics.delete(topic.id).await?;
    same(
        "decision",
        stores.decisions.decision(decided.id).await?,
        None,
    )?;
    stores.settle().await?;
    same("placement", stores.topics.placement(&placed).await?, None)?;
    same(
        "redirect",
        stores
            .topics
            .find(&TopicKey::parse("desktop-smoke")?)
            .await?,
        None,
    )?;
    let shown = stores.shown("desktop-smoke").await?;
    same("unbound", shown.topic_id, None)?;
    same("kept", shown.current_state.as_str(), "Smoke notes.")?;
    let hosts = stores.create(new_topic("Hosts", &[], None)?).await?;
    stores.topics.delete(hosts.id).await?;
    same(
        "tombstone",
        stores
            .topics
            .tombstoned(TopicKind::Work, &hosts.slug)
            .await?,
        true,
    )?;
    Ok(())
}

pub async fn editing_an_unwritten_entity_writes_its_page(stores: PageStores<'_>) -> Outcome {
    let hosts = stores
        .topics
        .find(&TopicKey::Slug(TopicKind::Type, Slug::fold("Hosts")?))
        .await?
        .ok_or("the Hosts type is seeded")?;
    let entity = stores
        .topics
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Entity,
                name: TopicName::new("AcmeKit build box")?,
                aliases: Vec::new(),
                parent: Some(hosts.id),
                directory: None,
                owner: Some(crate::owner()?),
            },
            at(1),
        )?)
        .await?;
    same("unwritten", stores.topics.page_slug(entity.id).await?, None)?;
    let edit = PageEdit {
        current_state: Some("Builds AcmeKit every night.".to_owned()),
        article: None,
    };
    let slug = edit_topic_page(stores.topics, stores.pages, &entity, &edit, at(2))
        .await?
        .ok_or("an entity owns a page")?;
    stores.settle().await?;
    let page = stores.shown(slug.as_ref()).await?;
    same(
        "written",
        page.current_state.as_str(),
        "Builds AcmeKit every night.",
    )?;
    same("title", page.title.as_str(), "AcmeKit build box")?;
    same("bound", page.topic_id, Some(entity.id.to_string()))?;
    same(
        "a type has no page",
        edit_topic_page(stores.topics, stores.pages, &hosts, &edit, at(3)).await?,
        None,
    )?;
    Ok(())
}

use std::str::FromStr;

use async_trait::async_trait;
use kernel::filing::FilingStore;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::transcript::ListLimit;
use kernel::transcript::SessionLister;
use kernel::transcript::SessionRenameError;
use kernel::transcript::StoreSession;
use kernel::transcript::ToolOutputRead;
use kernel::transcript::TranscriptRead;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptWindow;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::HeldEntity;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::builder::WikiWork;
use kernel::wiki::merge::PageMerger;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::route::TopicPorts;
use kernel::wiki::route::WikiRouter;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::topic_pages::PageStores;
use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;

pub const SESSION_KEY: &str = "claude-code:00000000-0000-4000-8000-000000000042";
pub const PARENT_KEY: &str = "claude-code:00000000-0000-4000-8000-000000000041";
pub const SESSION_TITLE: &str = "AcmeKit release prep";

type Found<T> = Result<T, Box<dyn std::error::Error>>;

#[expect(
    missing_debug_implementations,
    reason = "the stores are kernel ports without Debug"
)]
#[derive(Clone, Copy)]
pub struct RouteStores<'a> {
    pub pages: PageStores<'a>,
    pub wiki: &'a dyn WikiBuilderStore,
    pub filing: &'a dyn FilingStore,
    pub lineage: &'a dyn SessionLister,
}

#[derive(Debug)]
pub struct NoParents;

#[async_trait]
impl SessionLister for NoParents {
    async fn list(&self, _harnesses: &[HarnessId], _limit: ListLimit) -> Vec<StoreSession> {
        Vec::new()
    }

    async fn describe(
        &self,
        _harness: HarnessId,
        _native: &str,
        _parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError> {
        Ok(None)
    }

    async fn store_cwd(
        &self,
        _harness: HarnessId,
        _native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(None)
    }

    async fn transcript(
        &self,
        _harness: HarnessId,
        _native: &str,
        _window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError> {
        Ok(TranscriptRead::NoSource)
    }

    async fn rename(
        &self,
        _harness: HarnessId,
        _native: &str,
        _title: &str,
    ) -> Result<StoreSession, SessionRenameError> {
        Err(SessionRenameError::Missing)
    }

    async fn tool_output(
        &self,
        _harness: HarnessId,
        _native: &str,
        _tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError> {
        Ok(ToolOutputRead::NoSource)
    }
}

impl RouteStores<'_> {
    pub(crate) async fn place(&self, topic: &Topic) -> Outcome {
        self.place_as(SESSION_KEY, topic).await
    }

    pub(crate) async fn place_as(&self, key: &str, topic: &Topic) -> Outcome {
        self.pages
            .topics
            .place(Placement {
                session: SessionId::from_str(key)?,
                topic: topic.id,
                by: PlacementSource::You,
                at: at(1),
                owner: Some(crate::owner()?),
            })
            .await?;
        Ok(())
    }

    pub(crate) fn router(&self) -> WikiRouter<'_> {
        self.router_merging(None)
    }

    pub(crate) fn router_with<'a>(&'a self, merger: &'a dyn PageMerger) -> WikiRouter<'a> {
        self.router_merging(Some(merger))
    }

    fn router_merging<'a>(&'a self, merger: Option<&'a dyn PageMerger>) -> WikiRouter<'a> {
        WikiRouter {
            wiki: self.wiki,
            topics: TopicPorts {
                topics: self.pages.topics,
                pages: self.pages.pages,
                filing: self.filing,
                lineage: self.lineage,
            },
            merger,
            now: at(20),
        }
    }

    pub(crate) async fn route(&self, job: &ExtractWiki, patches: Vec<WikiPatch>) -> Outcome {
        let router = self.router();
        let WikiWork::Pending(summary) = self.wiki.wiki_work(job).await? else {
            return Err("the summary was already extracted".into());
        };
        let target = router.target(&summary).await?;
        router
            .apply(job, &summary, target.as_ref(), patches)
            .await?;
        self.pages.settle().await
    }

    pub(crate) async fn notes(&self, topic: &Topic) -> Found<Vec<(String, String)>> {
        Ok(self
            .pages
            .page(topic)
            .await?
            .sections
            .into_iter()
            .map(|section| (section.heading, section.body))
            .collect())
    }

    async fn cited(&self, slug: &str) -> Found<Vec<String>> {
        Ok(self
            .pages
            .shown(slug)
            .await?
            .citations
            .into_iter()
            .map(|citation| citation.summary_id)
            .collect())
    }

    pub(crate) async fn named(&self, kind: TopicKind, name: &str) -> Found<Option<Topic>> {
        Ok(self
            .pages
            .topics
            .find(&TopicKey::Slug(kind, Slug::fold(name)?))
            .await?)
    }

    pub(crate) async fn mentioned_elsewhere(
        &self,
        job: &ExtractWiki,
        patch: &WikiPatch,
    ) -> Outcome {
        self.wiki
            .hold_entity(HeldEntity {
                key: Slug::fold(&patch.title)?,
                source: "claude-code:00000000-0000-4000-8000-000000000099".to_owned(),
                summary_id: job.summary_id,
                patch: patch.clone(),
                at: at(2),
            })
            .await?;
        Ok(())
    }
}

pub(crate) fn patch(slug: &str, title: &str, summary: &str) -> Found<WikiPatch> {
    Ok(WikiPatch {
        action: WikiAction::Create,
        slug: WikiSlug::try_from(slug.to_owned())?,
        title: title.to_owned(),
        aliases: Vec::new(),
        tags: Vec::new(),
        entities: Vec::new(),
        summary: summary.to_owned(),
        article: None,
        related: None,
        history_entry: None,
        entity_type: None,
    })
}

pub(crate) const HOST: &str = "acme-build";

pub(crate) fn host(text: &str) -> Found<WikiPatch> {
    let mut host = patch(HOST, HOST, text)?;
    host.entity_type = Some("Hosts".to_owned());
    Ok(host)
}

pub async fn a_placed_session_writes_its_topic_page_and_no_other_topics_page(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores
        .pages
        .create(new_topic("Billing", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    let state = "AcmeKit ships every Friday.";
    stores
        .route(
            job,
            vec![
                patch("acmekit", "AcmeKit deploys", state)?,
                patch("rivet-billing", "Billing", "Invoices go out monthly.")?,
            ],
        )
        .await?;
    let home = stores.pages.shown("acmekit").await?;
    same("topic page state", home.current_state.as_str(), state)?;
    same("topic page title", home.title.as_str(), "AcmeKit")?;
    same(
        "other topic page",
        stores.pages.shown("billing").await?.current_state,
        String::new(),
    )?;
    same(
        "no loose page",
        stores.pages.lookup("rivet-billing").await?,
        WikiLookup::Missing,
    )?;
    let notes: Vec<(Option<TopicId>, String, String)> = stores
        .pages
        .page(&acmekit)
        .await?
        .sections
        .into_iter()
        .map(|section| (section.topic, section.heading, section.body))
        .collect();
    same(
        "note",
        notes,
        vec![(
            None,
            "Billing".to_owned(),
            "Invoices go out monthly.".to_owned(),
        )],
    )?;
    Ok(())
}

pub async fn the_topic_page_is_cited_without_a_proposal(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let topic = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place(&topic).await?;
    stores.route(job, Vec::new()).await?;
    let cited: Vec<String> = stores
        .pages
        .shown("acmekit")
        .await?
        .citations
        .into_iter()
        .map(|citation| citation.summary_id)
        .collect();
    same("citations", cited, vec![job.summary_id.to_string()])?;
    Ok(())
}

pub async fn a_second_session_naming_an_entity_gives_it_its_own_page(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let text = "The build box runs the nightly AcmeKit builds.";
    let host = host(text)?;
    stores.mentioned_elsewhere(job, &host).await?;
    stores.route(job, vec![host]).await?;
    let hosts = stores
        .named(TopicKind::Type, "Hosts")
        .await?
        .ok_or("the Hosts type is seeded")?;
    let entity = stores
        .named(TopicKind::Entity, HOST)
        .await?
        .ok_or("the second mention creates the entity")?;
    same("type", entity.parent, Some(hosts.id))?;
    let page = stores.pages.page(&entity).await?;
    same("own page", page.summary.as_str(), text)?;
    same(
        "type page",
        stores.pages.topics.page_slug(hosts.id).await?,
        None,
    )?;
    Ok(())
}

pub async fn an_unfiled_session_waits_until_it_is_filed(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let note = "Invoices go out monthly.";
    stores
        .route(
            job,
            vec![patch("acmekit-billing", "Billing", note)?, host("A box.")?],
        )
        .await?;
    same(
        "held entity",
        stores.named(TopicKind::Entity, HOST).await?,
        None,
    )?;
    same(
        "no loose page",
        stores.pages.lookup("acmekit-billing").await?,
        WikiLookup::Missing,
    )?;
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    stores.router().apply_deferred().await?;
    let sections: Vec<(String, String)> = stores
        .pages
        .page(&acmekit)
        .await?
        .sections
        .into_iter()
        .map(|section| (section.heading, section.body))
        .collect();
    same(
        "filed note",
        sections,
        vec![("Billing".to_owned(), note.to_owned())],
    )?;
    let cited: Vec<String> = stores
        .pages
        .shown("acmekit")
        .await?
        .citations
        .into_iter()
        .map(|citation| citation.summary_id)
        .collect();
    same("cited", cited, vec![job.summary_id.to_string()])?;
    same(
        "applied once",
        stores.router().apply_deferred().await?,
        Vec::<String>::new(),
    )?;
    Ok(())
}

pub async fn a_child_session_waits_for_its_parent(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let note = "Invoices go out monthly.";
    stores
        .route(job, vec![patch("acmekit-billing", "Billing", note)?])
        .await?;
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place_as(PARENT_KEY, &acmekit).await?;
    stores.router().apply_deferred().await?;
    let sections: Vec<(String, String)> = stores
        .pages
        .page(&acmekit)
        .await?
        .sections
        .into_iter()
        .map(|section| (section.heading, section.body))
        .collect();
    same(
        "the child's note on its parent's topic",
        sections,
        vec![("Billing".to_owned(), note.to_owned())],
    )?;
    Ok(())
}

pub async fn a_moved_session_takes_its_notes_along(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let text = "Invoices go out monthly.";
    let note = vec![("Billing".to_owned(), text.to_owned())];
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    let desk = stores
        .pages
        .create(new_topic("Billing Desk", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    stores
        .route(job, vec![patch("acmekit-billing", "Billing", text)?])
        .await?;
    same("written", stores.notes(&acmekit).await?, note.clone())?;
    stores.place(&desk).await?;
    stores.router().apply_deferred().await?;
    same(
        "left the old topic",
        stores.notes(&acmekit).await?,
        Vec::new(),
    )?;
    same("moved", stores.notes(&desk).await?, note.clone())?;
    same("uncited", stores.cited("acmekit").await?, Vec::new())?;
    same(
        "cited where it went",
        stores.cited("billing-desk").await?,
        vec![job.summary_id.to_string()],
    )?;
    stores
        .pages
        .topics
        .unplace(&SessionId::from_str(SESSION_KEY)?)
        .await?;
    stores.router().apply_deferred().await?;
    same("held while unfiled", stores.notes(&desk).await?, Vec::new())?;
    stores.place(&acmekit).await?;
    stores.router().apply_deferred().await?;
    same("back once filed", stores.notes(&acmekit).await?, note)?;
    Ok(())
}

pub async fn a_deleted_entity_is_not_recreated(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let hosts = stores
        .named(TopicKind::Type, "Hosts")
        .await?
        .ok_or("the Hosts type is seeded")?;
    let entity = stores
        .pages
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Entity,
                name: TopicName::new(HOST)?,
                aliases: Vec::new(),
                parent: Some(hosts.id),
                directory: None,
                owner: Some(crate::owner()?),
            },
            at(0),
        )?)
        .await?;
    stores.pages.topics.delete(entity.id).await?;
    let host = host("Still here.")?;
    stores.mentioned_elsewhere(job, &host).await?;
    stores.route(job, vec![host]).await?;
    same(
        "tombstoned",
        stores.named(TopicKind::Entity, HOST).await?,
        None,
    )?;
    Ok(())
}

pub async fn a_sub_topic_summary_lands_in_its_section(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let infra = stores.pages.create(new_topic("Infra", &[], None)?).await?;
    let certificates = stores
        .pages
        .create(new_topic("Certificates", &[], Some(infra.id))?)
        .await?;
    stores.place(&certificates).await?;
    let text = "Certificates renew every week.";
    stores
        .route(job, vec![patch("infra", "Certificates", text)?])
        .await?;
    let page = stores.pages.page(&infra).await?;
    same("root summary", page.summary.as_str(), "")?;
    same(
        "section",
        page.sections
            .iter()
            .map(|section| (section.topic, section.body.as_str()))
            .collect::<Vec<_>>(),
        vec![(Some(certificates.id), text)],
    )?;
    Ok(())
}

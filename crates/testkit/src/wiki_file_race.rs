use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use jiff::Timestamp;
use kernel::topics::DecisionStore;
use kernel::topics::PageChange;
use kernel::topics::PageEdit;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicId;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageStore;
use kernel::topics::sync_topic_page;
use kernel::wiki::WikiLookup;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::ExtractEntities;
use kernel::wiki::entity_pass::FactState;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::PreparedFact;
use kernel::wiki::entity_pass::PreparedHistory;
use kernel::wiki::entity_pass::StoredRewrite;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiSlug;
use uuid::Uuid;
use wire::time::TimestampFormat as _;

use super::CONVERSATION;
use super::PageFileStore;
use super::at;
use super::uid;
use super::write_leaf;
use crate::topics::Outcome;

const RACE_ID: &str = "00000000-0000-4000-8000-0000000000e8";
const RACE_TIME: &str = "2026-10-13T12:00:00Z";
const SWEEP_TIME: &str = "2026-10-13T13:00:00Z";
const PROSE: &str = "The race box hosts nightly builds.";

pub async fn a_late_sweep_keeps_the_entity_page(
    store: &(impl PageFileStore + DecisionStore),
    wiki: &Path,
) -> Outcome {
    super::prepare_wiki(wiki)?;
    let view = commit_entity(store, RACE_ID, "racebox", PROSE, RACE_TIME).await?;
    let topic = TopicId::from_uuid(view.id);
    assert_eq!(store.page_slug(topic).await?, None);
    let racing = RacingEntity::new(
        store,
        view.id,
        entity_rewrite(RACE_ID, PROSE, "Noted racebox.", RACE_TIME)?,
        at(RACE_TIME)?,
    );
    sync_topic_page(
        store,
        &racing,
        store,
        &SyncTopicPage::topic(topic),
        SweepOwners::All,
        at(SWEEP_TIME)?,
    )
    .await?;
    assert!(
        racing.fired(),
        "the entity builder never ran inside the sweep"
    );
    let slug = WikiSlug::try_from("racebox".to_owned())?;
    assert_eq!(store.page_slug(topic).await?, Some(slug.clone()));
    let WikiLookup::Page(page) = store.wiki_page(&slug).await? else {
        return Err("the entity page row is gone".into());
    };
    assert_eq!(
        page.current_state, PROSE,
        "the sweep blanked the entity row"
    );
    let file = std::fs::read_to_string(wiki.join("topics").join("racebox.md"))?;
    assert!(
        file.contains(PROSE),
        "the sweep blanked the entity file: {file}"
    );
    Ok(())
}

#[expect(missing_debug_implementations)]
pub struct RacingEntity<'a, S> {
    store: &'a S,
    entity: Uuid,
    body: StoredRewrite,
    now: Timestamp,
    fired: AtomicBool,
}

impl<'a, S: PageFileStore> RacingEntity<'a, S> {
    pub fn new(store: &'a S, entity: Uuid, body: StoredRewrite, now: Timestamp) -> Self {
        Self {
            store,
            entity,
            body,
            now,
            fired: AtomicBool::new(false),
        }
    }

    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    async fn write_entity_page(&self) -> Result<(), WikiStoreError> {
        if self.fired.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let failed = |error| WikiStoreError::Query(Box::new(error));
        let mut lease = self.store.lease_entity(self.entity).await.map_err(failed)?;
        lease.apply(&self.body, self.now).await.map_err(failed)?;
        lease.release().await.map_err(failed)
    }
}

#[async_trait::async_trait]
impl<S: PageFileStore> TopicPageStore for RacingEntity<'_, S> {
    async fn topic_page(&self, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError> {
        self.store.topic_page(slug).await
    }

    async fn page_taken(&self, slug: &WikiSlug) -> Result<bool, WikiStoreError> {
        let taken = self.store.page_taken(slug).await?;
        self.write_entity_page().await?;
        Ok(taken)
    }

    async fn save_topic_page(
        &self,
        topic: TopicId,
        page: &TopicPage,
        now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        self.store.save_topic_page(topic, page, now).await
    }

    async fn create_topic_page(
        &self,
        topic: TopicId,
        page: &TopicPage,
        now: Timestamp,
    ) -> Result<bool, WikiStoreError> {
        self.store.create_topic_page(topic, page, now).await
    }

    async fn edit_page(
        &self,
        slug: &WikiSlug,
        edit: &PageEdit,
        now: Timestamp,
    ) -> Result<bool, WikiStoreError> {
        self.store.edit_page(slug, edit, now).await
    }

    async fn change_page(
        &self,
        root: TopicId,
        slug: &WikiSlug,
        change: &PageChange<'_>,
        now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        self.store.change_page(root, slug, change, now).await
    }

    async fn redirect_page(
        &self,
        from: &WikiSlug,
        to: &WikiSlug,
        now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        self.store.redirect_page(from, to, now).await
    }

    async fn fold_page(
        &self,
        child: &WikiSlug,
        into: &WikiSlug,
        mark: FoldMark,
        now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        self.store.fold_page(child, into, mark, now).await
    }

    async fn remove_page(&self, slug: &WikiSlug) -> Result<(), WikiStoreError> {
        self.store.remove_page(slug).await
    }
}

pub(super) async fn commit_entity(
    store: &impl PageFileStore,
    summary: &str,
    name: &str,
    prose: &str,
    when: &str,
) -> Result<EntityView, Box<dyn std::error::Error>> {
    write_leaf(store, summary, prose, when).await?;
    let id = uid(summary)?;
    let conversation = uid(CONVERSATION)?;
    let leaf = match store
        .entity_work(&ExtractEntities {
            summary_id: id,
            conversation_id: Some(conversation),
            session_at: None,
        })
        .await?
    {
        kernel::wiki::entity_pass::EntityWork::Pending(leaf) => leaf,
        kernel::wiki::entity_pass::EntityWork::Complete { entities } => {
            return Err(format!("summary {summary} was already an entity: {entities:?}").into());
        }
    };
    Ok(store
        .commit(
            std::slice::from_ref(&leaf),
            &PreparedEntity {
                entity_ref: None,
                name: name.to_owned(),
                type_name: "Tools".to_owned(),
                aliases: Vec::new(),
                facts: Vec::new(),
                history: Vec::new(),
            },
            at(when)?,
        )
        .await?
        .ok_or("entity commit was skipped")?)
}

pub(super) fn entity_rewrite(
    summary: &str,
    prose: &str,
    sentence: &str,
    when: &str,
) -> Result<StoredRewrite, Box<dyn std::error::Error>> {
    let id = uid(summary)?;
    let conversation = uid(CONVERSATION)?;
    Ok(StoredRewrite {
        summary: prose.to_owned(),
        article: format!("{prose} Kept on the bench."),
        facts: vec![PreparedFact {
            attribute: "place".to_owned(),
            value: "bench".to_owned(),
            as_of: at(when)?,
            status: FactState::Current,
            summary_id: id,
            conversation_id: Some(conversation),
        }],
        history: Some(PreparedHistory {
            summary_id: id,
            conversation_id: Some(conversation),
            session_on: at(when)?.utc_date(),
            sentence: sentence.to_owned(),
        }),
        owner: crate::OWNER.to_owned(),
        session_at: at(when)?,
        session_id: id,
        evidence: sentence.to_owned(),
    })
}

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

use super::super::builder::BackfillBudget;
use super::types::AppliedMark;
use super::types::CandidateQuery;
use super::types::EntityLeaf;
use super::types::EntityMerge;
use super::types::EntityPage;
use super::types::EntityProse;
use super::types::EntityStoreError;
use super::types::EntityView;
use super::types::EntityWork;
use super::types::ExtractEntities;
use super::types::FollowingLeaf;
use super::types::MatchCandidate;
use super::types::MatchDecision;
use super::types::MentionNote;
use super::types::ObserveContext;
use super::types::PreparedEntity;
use super::types::PublishOutcome;
use super::types::StoredRewrite;
use super::types::TranscriptMessage;
use super::types::TypeBrief;

#[async_trait]
pub trait EntityStore: Send + Sync {
    async fn entity_work(&self, job: &ExtractEntities) -> Result<EntityWork, EntityStoreError>;

    async fn following(
        &self,
        lead: &EntityLeaf,
        limit: u32,
    ) -> Result<Vec<FollowingLeaf>, EntityStoreError>;

    async fn observe_context(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<ObserveContext, EntityStoreError>;

    async fn transcript(
        &self,
        leaves: &[EntityLeaf],
    ) -> Result<Vec<TranscriptMessage>, EntityStoreError>;

    async fn commit(
        &self,
        leaves: &[EntityLeaf],
        prepared: &PreparedEntity,
        now: DateTime<Utc>,
    ) -> Result<Option<EntityView>, EntityStoreError>;

    async fn publish(
        &self,
        view: &EntityView,
        prose: &EntityProse,
        now: DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError>;

    async fn finish_entities(
        &self,
        leaves: &[Uuid],
        slugs: &[String],
        git_sha: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError>;

    async fn fail_entity(
        &self,
        summary_id: Uuid,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError>;

    async fn entity_backfill(
        &self,
        budget: BackfillBudget,
    ) -> Result<Vec<ExtractEntities>, EntityStoreError>;

    async fn stale_entities(&self, limit: u32) -> Result<Vec<ExtractEntities>, EntityStoreError>;

    async fn revive_dead_entities(&self, limit: u32) -> Result<u64, EntityStoreError>;

    async fn match_candidates(
        &self,
        query: &CandidateQuery,
    ) -> Result<Vec<MatchCandidate>, EntityStoreError>;

    async fn remember_embedding(
        &self,
        entity: Uuid,
        embedding: &[f32],
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError>;

    async fn hold_candidate(
        &self,
        summary: Uuid,
        prepared: &PreparedEntity,
        confidence: f32,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError>;

    async fn type_names(&self) -> Result<Vec<String>, EntityStoreError> {
        Ok(Vec::new())
    }

    async fn record_decision(
        &self,
        _summary: Uuid,
        _decision: &MatchDecision,
        _now: DateTime<Utc>,
    ) -> Result<(), EntityStoreError> {
        Ok(())
    }

    async fn type_briefs(&self) -> Result<Vec<TypeBrief>, EntityStoreError> {
        Ok(Vec::new())
    }

    async fn remember_type_alias(
        &self,
        _type_name: &str,
        _alias: &str,
    ) -> Result<(), EntityStoreError> {
        Ok(())
    }

    async fn merge_entities(
        &self,
        _left: Uuid,
        _right: Uuid,
        _now: DateTime<Utc>,
    ) -> Result<Option<EntityMerge>, EntityStoreError> {
        Ok(None)
    }

    async fn lease_entity(&self, id: Uuid) -> Result<Box<dyn EntityLease>, EntityStoreError>;
}

#[async_trait]
pub trait EntityLease: Send {
    async fn page(&mut self) -> Result<EntityPage, EntityStoreError>;

    async fn applied_through(&mut self) -> Result<Option<AppliedMark>, EntityStoreError> {
        Ok(None)
    }

    async fn apply(
        &mut self,
        body: &StoredRewrite,
        now: DateTime<Utc>,
    ) -> Result<PublishOutcome, EntityStoreError>;

    async fn record_mention(
        &mut self,
        note: &MentionNote,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, EntityStoreError>;

    async fn release(&mut self) -> Result<(), EntityStoreError>;
}

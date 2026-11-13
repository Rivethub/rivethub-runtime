use chrono::NaiveDate;
use kernel::wiki::entity_pass::EntityProse;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::EntityView;
use kernel::wiki::entity_pass::HistoryLine;
use kernel::wiki::entity_pass::snapshot_of;
use kernel::wiki::page::LastVerified;
use kernel::wiki::page::PageCitation;
use kernel::wiki::page::PageSource;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSourceKind;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;

use super::sql::HEAD;

#[derive(Debug, sqlx::FromRow)]
struct EntityHead {
    id: Uuid,
    name: String,
    slug: String,
    owner_user_id: Option<String>,
    type_name: Option<String>,
    type_slug: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct HistoryRow {
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    session_on: NaiveDate,
    sentence: String,
}

#[derive(Debug, sqlx::FromRow)]
struct AliasRow {
    alias: String,
}

pub(super) async fn load_view(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<EntityView, EntityStoreError> {
    let head = sqlx::query_as::<_, EntityHead>(HEAD)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(query_error)?
        .ok_or_else(|| EntityStoreError::SlugTaken(id.to_string()))?;
    let facts = super::claims::current_facts(tx, id).await?.facts;
    let history = sqlx::query_as::<_, HistoryRow>(
        "SELECT summary_id, conversation_id, session_on, sentence FROM rh_entity_history
          WHERE entity_id = $1 ORDER BY session_on DESC, summary_id DESC",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(query_error)?;
    let aliases = sqlx::query_as::<_, AliasRow>(
        "SELECT alias FROM rh_topic_aliases WHERE topic_id = $1 AND kind = 'entity' ORDER BY alias_key",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(query_error)?;
    let history: Vec<HistoryLine> = history
        .into_iter()
        .map(|line| HistoryLine {
            date: line.session_on,
            sentence: line.sentence,
            summary_id: line.summary_id,
            conversation_id: line.conversation_id,
        })
        .collect();
    let snapshot = snapshot_of(&facts, &history);
    let mut alias_text: Vec<String> = aliases.into_iter().map(|row| row.alias).collect();
    alias_text.retain(|alias| alias != &head.name);
    Ok(EntityView {
        id: head.id,
        name: head.name,
        slug: head.slug,
        type_name: head.type_name.unwrap_or_default(),
        type_slug: head.type_slug.unwrap_or_default(),
        aliases: alias_text,
        facts,
        history,
        snapshot,
        owner: head.owner_user_id,
    })
}

pub(super) fn page_from(
    view: &EntityView,
    prose: &EntityProse,
) -> Result<WikiPage, EntityStoreError> {
    let slug = kernel::wiki::page::WikiSlug::try_from(view.slug.clone())
        .map_err(|_| EntityStoreError::SlugTaken(view.slug.clone()))?;
    let mut page = WikiPage::empty(slug, view.name.clone());
    prose.summary.trim().clone_into(&mut page.current_state);
    page.article = kernel::wiki::entity_pass::article_with_facts(&prose.article, &view.facts);
    page.meta.aliases.clone_from(&view.aliases);
    page.meta.tags = page_tags(view);
    page.history = kernel::wiki::entity_pass::render_history(&view.history);
    page.meta.last_verified = view
        .facts
        .iter()
        .map(|fact| fact.as_of)
        .max()
        .map(LastVerified::from);
    page.meta.sources = page_sources(view);
    page.citations = view
        .history
        .iter()
        .map(|line| PageCitation {
            summary_id: line.summary_id.to_string(),
            date: Some(line.date),
            kind: None,
            note: None,
        })
        .collect();
    drop(kernel::wiki::page::bound::bound_page(None, &mut page));
    Ok(page)
}

fn page_tags(view: &EntityView) -> Vec<String> {
    let mut tags = Vec::new();
    if !view.type_slug.is_empty() {
        tags.push(view.type_slug.clone());
    }
    if !view.type_name.is_empty() && !tags.iter().any(|tag| tag == &view.type_name) {
        tags.push(view.type_name.clone());
    }
    tags
}

fn page_sources(view: &EntityView) -> Vec<PageSource> {
    let mut ids: Vec<String> = view
        .history
        .iter()
        .map(|line| line.summary_id.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Vec::new();
    }
    vec![PageSource {
        kind: WikiSourceKind::Summary,
        ids,
        conversation_id: view
            .history
            .iter()
            .find_map(|line| line.conversation_id)
            .map(|id| id.to_string()),
        extra: wire::Extra::default(),
    }]
}

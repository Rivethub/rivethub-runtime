use super::read_rows;
use super::state_of;
use super::store_error;
use jiff::Timestamp;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagState;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyEntry;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyMergeResult;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use sqlx::PgPool;
use sqlx::QueryBuilder;

#[derive(Debug, sqlx::FromRow)]
struct TagTaxonomyRow {
    key: String,
    value: String,
    display: String,
    parent_value: Option<String>,
    aliases: Vec<String>,
    state: String,
    source: String,
    reason: String,
    #[sqlx(try_from = "crate::time::OptionalTimestamp")]
    decided_at: Option<Timestamp>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    updated_at: Timestamp,
}

impl TryFrom<TagTaxonomyRow> for TagTaxonomyEntry {
    type Error = MemoryStoreError;

    fn try_from(row: TagTaxonomyRow) -> Result<Self, Self::Error> {
        Ok(Self {
            key: row.key,
            value: row.value,
            display: row.display,
            parent_value: row.parent_value,
            aliases: row.aliases,
            state: state_of(&row.state).map_err(MemoryStoreError::from)?,
            source: row.source,
            reason: row.reason,
            decided_at: row.decided_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

async fn list_tag_taxonomy(
    pool: &PgPool,
    query: &TagTaxonomyQuery,
) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
    let mut builder = QueryBuilder::new(
        "select key, value, display, parent_value, aliases, state, source, reason, decided_at, \
         created_at, updated_at from rh_tag_taxonomy where true",
    );
    if let Some(key) = &query.key {
        builder.push(" and key = ").push_bind(key.clone());
    }
    if let Some(state) = query.state {
        builder.push(" and state = ").push_bind(state.to_string());
    }
    builder
        .push(" order by key, value limit ")
        .push_bind(i64::from(query.limit.get()));
    let rows = read_rows(
        "taxonomy list",
        builder
            .build_query_as::<TagTaxonomyRow>()
            .fetch_all(pool)
            .await,
    )?;
    rows.shape()
        .into_iter()
        .map(TagTaxonomyEntry::try_from)
        .collect()
}

async fn upsert_tag_taxonomy(
    pool: &PgPool,
    upsert: &TagTaxonomyUpsert,
) -> Result<TagTaxonomyEntry, MemoryStoreError> {
    let record = sqlx::query!(
        r#"insert into rh_tag_taxonomy (key, value, display, parent_value, aliases, state, source, reason) values ($1, $2, coalesce($3, ''), $4, $5, 'accepted', 'user', coalesce($6, '')) on conflict (key, value) do update set display = coalesce($3, ''), parent_value = $4, aliases = $5, reason = coalesce($6, ''), updated_at = now() returning key, value, display, parent_value, aliases, state, source, reason, decided_at AS "decided_at!: crate::time::OptionalTimestamp", created_at AS "created_at!: jiff_sqlx::Timestamp", updated_at AS "updated_at!: jiff_sqlx::Timestamp""#,
        upsert.key,
        upsert.value,
        upsert.display.clone(),
        upsert.parent_value.clone(),
        &upsert.aliases,
        upsert.reason.clone()
    )
    .fetch_one(pool)
    .await
    .map_err(|source| store_error("taxonomy upsert", source))?;
    Ok(TagTaxonomyEntry {
        key: record.key,
        value: record.value,
        display: record.display,
        parent_value: record.parent_value,
        aliases: record.aliases,
        state: state_of(&record.state).map_err(MemoryStoreError::from)?,
        source: record.source,
        reason: record.reason,
        decided_at: record.decided_at.into(),
        created_at: record.created_at.into(),
        updated_at: record.updated_at.into(),
    })
}

async fn decide_tag_taxonomy(
    pool: &PgPool,
    decision: &TagTaxonomyDecision,
) -> Result<u64, MemoryStoreError> {
    if decision.entries.is_empty() {
        return Ok(0);
    }
    let mut builder = QueryBuilder::new("update rh_tag_taxonomy set state = ");
    let state = TagState::from(decision.decision);
    builder.push_bind(state.to_string());
    builder.push(", decided_at = now(), updated_at = now() where (key, value) in ");
    builder.push_tuples(
        decision
            .entries
            .iter()
            .map(|entry| (&entry.key, &entry.value)),
        |mut separated, (key, value)| {
            separated.push_bind(key.as_str());
            separated.push_bind(value.as_str());
        },
    );
    let result = builder
        .build()
        .execute(pool)
        .await
        .map_err(|source| store_error("taxonomy decide", source))?;
    Ok(result.rows_affected())
}

async fn merge_tag_taxonomy(
    pool: &PgPool,
    merge: &TagTaxonomyMerge,
) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
    if merge.from == merge.into {
        return Ok(TagTaxonomyMergeResult {
            moved: 0,
            dropped: 0,
            into: merge.into.clone(),
        });
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|source| store_error("taxonomy merge", source))?;
    let dropped_rows = sqlx::query_scalar!(
        "delete from rh_tags t where t.key = $1 and t.value = $2 and exists \
           (select 1 from rh_tags k where k.key = $1 and k.value = $3 \
              and k.entity_type = t.entity_type and k.entity_id = t.entity_id) returning 1",
        merge.key,
        merge.from,
        merge.into
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|source| store_error("taxonomy merge", source))?;
    let moved = sqlx::query!(
        "update rh_tags set value = $3, updated_at = now() where key = $1 and value = $2",
        merge.key,
        merge.from,
        merge.into
    )
    .execute(&mut *tx)
    .await
    .map_err(|source| store_error("taxonomy merge", source))?
    .rows_affected();
    sqlx::query!(
        "update rh_tag_taxonomy set aliases = array_append(aliases, $2), updated_at = now() \
          where key = $1 and value = $3 and not ($2 = any(aliases))",
        merge.key,
        merge.from,
        merge.into
    )
    .execute(&mut *tx)
    .await
    .map_err(|source| store_error("taxonomy merge", source))?;
    sqlx::query!(
        "delete from rh_tag_taxonomy where key = $1 and value = $2",
        merge.key,
        merge.from
    )
    .execute(&mut *tx)
    .await
    .map_err(|source| store_error("taxonomy merge", source))?;
    tx.commit()
        .await
        .map_err(|source| store_error("taxonomy merge", source))?;
    Ok(TagTaxonomyMergeResult {
        moved,
        dropped: u64::try_from(dropped_rows.len()).map_err(crate::Error::from)?,
        into: merge.into.clone(),
    })
}

impl crate::PostgresMemory {
    pub(crate) async fn tags_taxonomy(
        &self,
        query: &TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
        list_tag_taxonomy(&self.pool, query).await
    }

    pub(crate) async fn tags_taxonomy_upsert(
        &self,
        upsert: &TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError> {
        upsert_tag_taxonomy(&self.pool, upsert).await
    }

    pub(crate) async fn tags_taxonomy_decide(
        &self,
        decision: &TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        decide_tag_taxonomy(&self.pool, decision).await
    }

    pub(crate) async fn tags_taxonomy_merge(
        &self,
        merge: &TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
        merge_tag_taxonomy(&self.pool, merge).await
    }
}

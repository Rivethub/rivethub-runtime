use std::str::FromStr;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::agent_presets::AgentPreset;
use kernel::agent_presets::AgentPresetStore;
use kernel::agent_presets::AgentPresetStoreError;
use kernel::agent_presets::AgentPresetUpdate;
use kernel::agent_presets::AgentStoreBackend;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::postgres::PgTypeInfo;
use sqlx::postgres::PgValueRef;
use wire::harness::HarnessId;

use crate::Error;
use crate::PostgresMemory;

const NAME_INDEX: &str = "idx_ros_agent_presets_name";

#[derive(Debug, thiserror::Error)]
#[error("agent preset {0} is required")]
struct Required(&'static str);

#[derive(Debug, Clone, Copy)]
struct StoredHarness(HarnessId);

impl sqlx::Type<Postgres> for StoredHarness {
    fn type_info() -> PgTypeInfo {
        <String as sqlx::Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as sqlx::Type<Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, Postgres> for StoredHarness {
    fn decode(value: PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let text = <&str as sqlx::Decode<'r, Postgres>>::decode(value)?;
        Ok(Self(HarnessId::from_str(text)?))
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PresetRow {
    id: String,
    name: String,
    color: String,
    harness_id: Option<StoredHarness>,
    model: String,
    effort: String,
    system_prompt: String,
    node: String,
    directory: String,
    shared_link: bool,
    node_base_url: String,
    sort_order: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PostgresPresets {
    pool: PgPool,
}

impl PostgresPresets {
    pub async fn detect(pool: PgPool) -> Result<Option<Self>, Error> {
        let present = sqlx::query_scalar!(
            r#"SELECT to_regclass('rh_agent_presets') IS NOT NULL AS "present!""#
        )
        .fetch_one(&pool)
        .await
        .map_err(|source| Error::operation("agent preset table probe", source))?;
        Ok(present.then_some(Self { pool }))
    }
}

impl PostgresMemory {
    pub async fn agent_presets(&self) -> Result<Option<PostgresPresets>, Error> {
        if self.unverified {
            return Ok(Some(PostgresPresets {
                pool: self.pool.clone(),
            }));
        }
        PostgresPresets::detect(self.pool.clone()).await
    }
}

#[async_trait]
impl AgentPresetStore for PostgresPresets {
    fn backend(&self) -> AgentStoreBackend {
        AgentStoreBackend::Postgres
    }

    async fn list(&self, node: Option<&str>) -> Result<Vec<AgentPreset>, AgentPresetStoreError> {
        let rows = sqlx::query_as!(
            PresetRow,
            r#"SELECT id, name, color, harness_id AS "harness_id: StoredHarness", model, effort,
                    system_prompt, node, directory, shared_link, node_base_url, sort_order,
                    created_at, updated_at
             FROM rh_agent_presets
             WHERE ($1::text IS NULL OR node = $1)
             ORDER BY created_at ASC, id ASC"#,
            node
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| failed("agent preset list", source))?;
        rows.into_iter().map(preset).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<AgentPreset>, AgentPresetStoreError> {
        let row = sqlx::query_as!(
            PresetRow,
            r#"SELECT id, name, color, harness_id AS "harness_id: StoredHarness", model, effort,
                    system_prompt, node, directory, shared_link, node_base_url, sort_order,
                    created_at, updated_at
             FROM rh_agent_presets
             WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| failed("agent preset read", source))?;
        row.map(preset).transpose()
    }

    async fn lookup(&self, id_or_name: &str) -> Result<Option<AgentPreset>, AgentPresetStoreError> {
        let row = sqlx::query_as!(
            PresetRow,
            r#"SELECT id, name, color, harness_id AS "harness_id: StoredHarness", model, effort,
                    system_prompt, node, directory, shared_link, node_base_url, sort_order,
                    created_at, updated_at
             FROM rh_agent_presets
             WHERE id = $1 OR lower(name) = lower($1)
             ORDER BY (id = $1) DESC
             LIMIT 1"#,
            id_or_name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| failed("agent preset lookup", source))?;
        row.map(preset).transpose()
    }

    async fn create(&self, preset: AgentPreset) -> Result<AgentPreset, AgentPresetStoreError> {
        let node = preset.node.as_deref().ok_or_else(|| required("node"))?;
        let directory = preset
            .directory
            .as_deref()
            .ok_or_else(|| required("directory"))?;
        let harness = preset.harness_id.map(|id| id.to_string());
        let sort_order = sort_column(preset.sort_order)?;
        let row = sqlx::query_as!(
            PresetRow,
            r#"INSERT INTO rh_agent_presets (
                 id, name, color, harness_id, model, effort, system_prompt, node, directory,
                 shared_link, node_base_url, sort_order, created_at, updated_at
             ) VALUES (
                 $1, $2, $3, $4, $5, $6, $7, $8, $9,
                 COALESCE($10, true), COALESCE($11, ''), $12, $13, $14
             )
             RETURNING id, name, color, harness_id AS "harness_id: StoredHarness", model, effort,
                       system_prompt, node, directory, shared_link, node_base_url, sort_order,
                    created_at, updated_at"#,
            preset.id,
            preset.name,
            preset.color,
            harness,
            preset.model,
            preset.effort,
            preset.system_prompt,
            node,
            directory,
            preset.shared_link,
            preset.node_base_url,
            sort_order,
            preset.created_at,
            preset.updated_at
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|source| written("agent preset create", &preset.name, source))?;
        self::preset(row)
    }

    async fn update(
        &self,
        id: &str,
        changes: AgentPresetUpdate,
    ) -> Result<Option<AgentPreset>, AgentPresetStoreError> {
        let harness = changes
            .harness_id
            .map(|slot| slot.map(|harness| harness.to_string()));
        let sort_order = match changes.sort_order {
            Some(slot) => Some(sort_column(slot)?),
            None => None,
        };
        let attempted = changes.name.clone();
        let row = sqlx::query_as!(
            PresetRow,
            r#"UPDATE rh_agent_presets SET
                 name = COALESCE($2, name),
                 color = COALESCE($3, color),
                 harness_id = CASE WHEN $4::boolean THEN $5::text ELSE harness_id END,
                 model = COALESCE($6, model),
                 effort = COALESCE($7, effort),
                 system_prompt = COALESCE($8, system_prompt),
                 directory = COALESCE($9, directory),
                 shared_link = COALESCE($10, shared_link),
                 node_base_url = COALESCE($11, node_base_url),
                 sort_order = CASE WHEN $12::boolean THEN $13::integer ELSE sort_order END,
                 updated_at = now()
             WHERE id = $1
             RETURNING id, name, color, harness_id AS "harness_id: StoredHarness", model, effort,
                       system_prompt, node, directory, shared_link, node_base_url, sort_order,
                    created_at, updated_at"#,
            id,
            changes.name,
            changes.color,
            harness.is_some(),
            harness.flatten(),
            changes.model,
            changes.effort,
            changes.system_prompt,
            changes.directory,
            changes.shared_link,
            changes.node_base_url,
            sort_order.is_some(),
            sort_order.flatten()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| match &attempted {
            Some(name) => written("agent preset update", name, source),
            None => failed("agent preset update", source),
        })?;
        row.map(preset).transpose()
    }

    async fn delete(&self, id: &str) -> Result<bool, AgentPresetStoreError> {
        let done = sqlx::query!("DELETE FROM rh_agent_presets WHERE id = $1", id)
            .execute(&self.pool)
            .await
            .map_err(|source| failed("agent preset delete", source))?;
        Ok(done.rows_affected() > 0)
    }
}

fn preset(row: PresetRow) -> Result<AgentPreset, AgentPresetStoreError> {
    let sort_order = match row.sort_order {
        Some(value) => Some(u32::try_from(value).map_err(|source| {
            AgentPresetStoreError::Unavailable(Box::new(Error::stored("sort_order", source)))
        })?),
        None => None,
    };
    Ok(AgentPreset {
        id: row.id,
        name: row.name,
        color: row.color,
        harness_id: row.harness_id.map(|stored| stored.0),
        model: row.model,
        effort: row.effort,
        system_prompt: row.system_prompt,
        node: Some(row.node),
        directory: Some(row.directory),
        shared_link: Some(row.shared_link),
        node_base_url: Some(row.node_base_url),
        sort_order,
        created_at: row.created_at,
        updated_at: row.updated_at,
        extra: wire::Extra::default(),
    })
}

fn sort_column(value: Option<u32>) -> Result<Option<i32>, AgentPresetStoreError> {
    value
        .map(i32::try_from)
        .transpose()
        .map_err(|source| AgentPresetStoreError::Unavailable(Box::new(Error::Integer(source))))
}

fn required(field: &'static str) -> AgentPresetStoreError {
    AgentPresetStoreError::Unavailable(Box::new(Required(field)))
}

fn written(operation: &'static str, name: &str, source: sqlx::Error) -> AgentPresetStoreError {
    let duplicate = source
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation() && error.constraint() == Some(NAME_INDEX));
    if duplicate {
        AgentPresetStoreError::NameTaken {
            name: name.to_owned(),
        }
    } else {
        failed(operation, source)
    }
}

fn failed(operation: &'static str, source: sqlx::Error) -> AgentPresetStoreError {
    AgentPresetStoreError::Unavailable(Box::new(Error::operation(operation, source)))
}

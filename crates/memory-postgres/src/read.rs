use crate::error::Error;
use crate::memory_jobs::ToolArguments;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory_core::TimeRange;
use std::str::FromStr as _;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

#[derive(sqlx::FromRow)]
pub(crate) struct MessageEntryRow {
    pub(crate) id: Option<String>,
    pub(crate) session_key: String,
    pub(crate) agent: String,
    pub(crate) channel: String,
    pub(crate) role_text: Option<String>,
    pub(crate) content: String,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_args: Option<String>,
    pub(crate) tool_result: Option<String>,
    pub(crate) metadata: Option<String>,
    pub(crate) created_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub(crate) import_settings: Option<String>,
}

impl TryFrom<MessageEntryRow> for MemoryEntry {
    type Error = Error;

    fn try_from(row: MessageEntryRow) -> Result<Self, Self::Error> {
        let role_text = row
            .role_text
            .as_deref()
            .ok_or_else(|| Error::protocol("message row is missing its role"))?;
        let role =
            Role::from_str(role_text).map_err(|source| Error::stored("message role", source))?;
        Ok(Self {
            id: row.id,
            session_id: row.session_key,
            agent: row.agent,
            channel: row.channel,
            role,
            content: row.content,
            tool_name: row.tool_name,
            tool_args: stored_tool_args(row.tool_args)?,
            tool_result: row.tool_result,
            metadata: stored_json(row.metadata, "metadata")?,
            created_at: row.created_at,
            provenance: None,
        })
    }
}

impl MessageEntryRow {
    pub(crate) fn into_entry(
        mut self,
        seen: &mut std::collections::HashMap<String, Option<kernel::memory::Provenance>>,
    ) -> Result<MemoryEntry, Error> {
        let settings = self.import_settings.take();
        let mut entry = MemoryEntry::try_from(self)?;
        if let Some(text) = settings {
            entry.provenance = if let Some(known) = seen.get(&entry.session_id) {
                known.clone()
            } else {
                let parsed = stored_json(Some(text), "settings")?
                    .as_ref()
                    .map(kernel::memory::Provenance::from_settings);
                seen.insert(entry.session_id.clone(), parsed.clone());
                parsed
            };
        }
        Ok(entry)
    }
}

fn stored_tool_args(raw: Option<String>) -> Result<Option<JsonObject>, Error> {
    let Some(text) = raw else {
        return Ok(None);
    };
    let legacy =
        match serde_json::from_str(&text).map_err(|source| Error::stored("tool_args", source))? {
            ToolArguments::Object(arguments) => return Ok(Some(arguments)),
            ToolArguments::Encoded(legacy) => legacy,
        };
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let decoded = serde_json::from_str(&legacy).ok();
    Ok(decoded)
}

pub(crate) fn readable_entries(rows: Vec<MessageEntryRow>) -> Vec<MemoryEntry> {
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.id.clone();
        match MemoryEntry::try_from(row) {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                tracing::warn!(id = id.as_deref(), %error, "skipping unreadable memory row");
            }
        }
    }
    entries
}

fn stored_json(raw: Option<String>, column: &'static str) -> Result<Option<JsonObject>, Error> {
    let Some(text) = raw else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| Error::stored(column, source))
}

fn filter_bounds(filter: &BrowseFilter) -> Result<TimeRange, Error> {
    if filter.window.is_some() && filter.since.is_none() && filter.before.is_none() {
        return Err(Error::UnresolvedWindow);
    }
    Ok(TimeRange {
        since: filter.since,
        before: filter.before,
    })
}

fn push_tag_condition(
    query: &mut sqlx::QueryBuilder<sqlx::Postgres>,
    tag: &kernel::memory::TagLiteral,
) {
    query.push(" and m.conversation_id in (select entity_id from rh_tags where key = ");
    query.push_bind(tag.key.clone());
    query.push(" and value = ");
    query.push_bind(tag.value.clone());
    query.push(" and state = 'accepted' and entity_type = 'conversation')");
}

impl crate::PostgresMemory {
    pub(crate) async fn browse_port(
        &self,
        filter: &BrowseFilter,
    ) -> Result<Vec<MemoryEntry>, Error> {
        let mut query = sqlx::QueryBuilder::new(
            "select m.id::text as id, c.session_key, m.agent, m.channel, m.role as role_text, \
             m.content, m.tool_name, m.tool_args::text as tool_args, m.tool_result, \
             m.metadata::text as metadata, m.created_at, \
             case when c.session_key like 'import:%' then coalesce(c.settings::text, '{}') end as import_settings \
             from rh_messages m join rh_conversations c on c.id = m.conversation_id where true",
        );
        query.push(crate::session_marks::not_trashed("m"));
        if !filter.include_tools {
            query.push(" and m.role <> 'tool'");
        }
        if let Some(conversation_id) = filter.conversation_id {
            query.push(" and m.conversation_id = ");
            query.push_bind(conversation_id);
        }
        if let Some(agent) = &filter.agent {
            query.push(" and m.agent = ");
            query.push_bind(agent.clone());
        }
        if let Some(role) = filter.role {
            query.push(" and m.role = ");
            query.push_bind(role.to_string());
        }
        if let Some(tool_name) = &filter.tool_name {
            query.push(" and m.tool_name = ");
            query.push_bind(tool_name.clone());
        }
        if let Some(tag) = &filter.tag {
            push_tag_condition(&mut query, tag);
        }
        let bounds = filter_bounds(filter)?;
        if let Some(since) = bounds.since {
            query.push(" and m.created_at >= ");
            query.push_bind(since);
        }
        if let Some(before) = bounds.before {
            query.push(" and m.created_at < ");
            query.push_bind(before);
        }
        let direction = match filter.order {
            MemoryBrowseOrder::Asc => "asc",
            MemoryBrowseOrder::Desc => "desc",
        };
        query.push(format!(
            " order by m.created_at {direction} limit {}",
            i64::from(filter.limit.get())
        ));
        let rows = query
            .build_query_as::<MessageEntryRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("browse", source))?;
        let mut seen = std::collections::HashMap::new();
        rows.into_iter()
            .map(|row| row.into_entry(&mut seen))
            .collect()
    }

    pub async fn memory_get_full(&self, id: &str) -> Result<Option<MemoryEntry>, Error> {
        let uuid = Uuid::parse_str(id)?;
        let record = sqlx::query_as!(
            MessageEntryRow,
            "select m.id::text as id, c.session_key as \"session_key!\", \
             m.agent as \"agent!\", m.channel as \"channel!\", m.role as role_text, \
             m.content as \"content!\", m.tool_name, m.tool_args::text as tool_args, \
             m.tool_result, m.metadata::text as metadata, m.created_at, \
             case when c.session_key like 'import:%' then coalesce(c.settings::text, '{}') end as import_settings \
             from rh_messages m join rh_conversations c on c.id = m.conversation_id \
             where m.id = $1",
            uuid
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("get full", source))?
        .map(|row| row.into_entry(&mut std::collections::HashMap::new()))
        .transpose()?;
        Ok(record)
    }
}

use jiff::Timestamp;
use kernel::memory::MemoryScope;
use kernel::memory::{Limit, SearchMode, SearchQuery};
use memory_sqlite::{QueryEmbed, SqliteStore};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use testkit::fakes::FakeProvider;
use tokio_util::sync::CancellationToken;

pub(crate) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub(crate) fn query(text: &str, mode: SearchMode) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        mode,
        scope: MemoryScope::Both,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(10), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

pub(crate) fn store(
    provider: Option<Arc<FakeProvider>>,
) -> TestResult<(tempfile::TempDir, SqliteStore)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let mut store = SqliteStore::open(&path)?;
    if let Some(provider) = provider {
        store = store.with_query_embed(QueryEmbed::new(
            provider,
            kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
            Duration::from_secs(1),
            CancellationToken::new(),
        ));
    }
    Ok((dir, store))
}

#[derive(Deserialize)]
pub(crate) struct Recording {
    pub(crate) hybrid: Hybrid,
}
#[serde_with::serde_as]
#[derive(Deserialize)]
pub(crate) struct Hybrid {
    #[serde_as(as = "wire::time::Rfc3339")]
    pub(crate) clock: Timestamp,
    pub(crate) vectors: Vec<Vector>,
    pub(crate) summary_vectors: Vec<Vector>,
    #[serde(rename = "memorySearch")]
    pub(crate) memory: Vec<Case>,
    #[serde(rename = "backendSearch")]
    pub(crate) backend: Vec<BackendCase>,
    pub(crate) literal_and_degraded: Vec<Case>,
    pub(crate) access_after: Vec<AccessRow>,
    pub(crate) summary_access_after: Vec<AccessRow>,
}
#[derive(Deserialize)]
pub(crate) struct Vector {
    pub(crate) id: String,
    pub(crate) embedding_hex: String,
}
#[derive(Deserialize)]
pub(crate) struct Case {
    pub(crate) query: String,
    pub(crate) options: Options,
    pub(crate) hits: Vec<Hit>,
    pub(crate) degraded: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Options {
    pub(crate) scope: Option<MemoryScope>,
    pub(crate) agent: Option<String>,
    pub(crate) limit: Option<u32>,
    pub(crate) tag: Option<String>,
}
#[serde_with::serde_as]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Hit {
    pub(crate) id: String,
    pub(crate) content: String,
    pub(crate) agent: String,

    #[serde_as(as = "wire::time::Rfc3339")]
    pub(crate) created_at: Timestamp,
    #[serde(alias = "score")]
    pub(crate) relevance_score: f64,
    #[serde(alias = "layer")]
    pub(crate) source: kernel::memory::MemoryHitSource,
    pub(crate) role: HitRole,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum HitRole {
    Message(wire::message::Role),
    Summary(kernel::memory::SummaryKind),
}

pub(crate) async fn seed(store: &SqliteStore, recording: &Hybrid) -> TestResult {
    let vectors: Vec<_> = recording
        .vectors
        .iter()
        .chain(&recording.summary_vectors)
        .map(|entry| Ok((entry.id.clone(), hex::decode(&entry.embedding_hex)?)))
        .collect::<TestResult<_>>()?;
    store.with_conn(move |conn| {
        let at = "2026-10-10T12:00:00.000Z";
        for (id, agent) in [("deploy", "rivet"), ("helper", "helper"), ("rollback", "rivet")] {
            conn.execute("INSERT INTO ros_conversations(id, session_key, agent, channel, created_at, updated_at) VALUES (?1,?1,?2,'cli',?3,?3)", rusqlite::params![id,agent,at]).map_err(failed)?;
        }
        for (suffix, conversation, role, content, tool, stamp) in [
            (2, "deploy", "user", "how do we deploy the AcmeKit service to staging", None, at),
            (1, "deploy", "assistant", "run the deploy script from the release branch, then watch the health check", None, "2026-10-04T08:00:00.000Z"),
            (4, "deploy", "tool", "", Some("deployed build 42 to staging.example.com"), at),
            (11,"helper","user","notes about the staging database migration on db.example.com",None,"2026-10-10T12:01:00.000Z"),
            (5,"helper","assistant","config lives at /home/user/work/acme-app/config.yaml and listens on api.example.com:8443",None,"2026-10-10T12:01:00.000Z"),
            (14,"helper","user","ok",None,"2026-10-10T12:01:00.000Z"),
            (15,"helper","tool","",Some("build_id=acme_42%done"),"2026-10-10T12:01:00.000Z"),
            (16,"rollback","user","rollback rollback rollback procedure notes",None,"2026-09-01T00:00:00.000Z"),
            (17,"rollback","assistant","the rollback switches traffic back to the previous colour",None,"2026-10-09T00:00:00.000Z"),
        ] {
            let id = format!("00000000-0000-4000-8000-{suffix:012}");
            let agent = if conversation == "helper" { "helper" } else { "rivet" };
            conn.execute("INSERT INTO ros_messages(id,conversation_id,role,content,agent,channel,created_at,tool_name,tool_result) VALUES (?1,?2,?3,?4,?5,'cli',?6,?7,?8)",
                rusqlite::params![id,conversation,role,content,agent,stamp,tool.map(|_| "Bash"),tool]).map_err(failed)?;
        }
        for (id, content, kind, stamp, conversation) in [
            ("00000000-0000-4000-8000-400000000001", "The team decided to deploy the AcmeKit service with a blue-green rollout. The release branch is cut on Mondays, the deploy script runs from the release branch, and a health check on staging.example.com gates the traffic switch. Rollback is a traffic switch back to the previous colour.", "leaf", "2026-10-10T12:00:30.000Z", Some("deploy")),
            ("00000000-0000-4000-8000-400000000002", "Branch: staging deploys and database migration plans for AcmeKit", "branch", "2026-10-10T12:00:40.000Z", None),
        ] {
            conn.execute("INSERT INTO ros_summaries(id,content,kind,created_at,conversation_id) VALUES (?1,?2,?3,?4,?5)", rusqlite::params![id,content,kind,stamp,conversation]).map_err(failed)?;
        }
        for (id, bytes) in vectors {
            let messages = conn.execute("UPDATE ros_messages SET embedding=?2 WHERE id=?1", rusqlite::params![id,bytes]).map_err(failed)?;
            if messages == 0 { conn.execute("UPDATE ros_summaries SET embedding=?2 WHERE id=?1",rusqlite::params![id,bytes]).map_err(failed)?; }
        }
        for (id, entity_type, entity_id, key, value) in [("acme","conversation","deploy","project","acmekit"),("infra","conversation","helper","project","infra"),("topic","summary","00000000-0000-4000-8000-400000000001","topic","deploys")] {
            conn.execute("INSERT INTO ros_tags(id,entity_type,entity_id,key,value,source,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'user','accepted',?6,?6)",rusqlite::params![id,entity_type,entity_id,key,value,at]).map_err(failed)?;
        }
        conn.execute("UPDATE ros_messages SET access_count=7 WHERE id='00000000-0000-4000-8000-000000000017'",[]).map_err(failed)?;
        Ok(())
    }).await?;
    Ok(())
}

pub(crate) async fn reinforce(
    store: &SqliteStore,
    ids: Vec<String>,
    clock: Timestamp,
) -> Result<(), memory_sqlite::SqliteError> {
    let stamp = clock.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    store
        .with_conn(move |conn| {
            for id in &ids {
                for table in ["ros_messages", "ros_summaries"] {
                    conn.execute(
                        &format!(
                            "UPDATE {table} SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2"
                        ),
                        rusqlite::params![stamp, id],
                    )
                    .map_err(failed)?;
                }
            }
            Ok(())
        })
        .await
}

pub(crate) fn failed(source: rusqlite::Error) -> memory_sqlite::SqliteError {
    memory_sqlite::SqliteError::Query {
        operation: "test fixture",
        source,
    }
}

#[derive(Deserialize)]
pub(crate) struct BackendCase {
    pub(crate) query: String,
    pub(crate) options: Options,
    pub(crate) response: Option<BackendResponse>,
}
#[derive(Deserialize)]
pub(crate) struct BackendResponse {
    pub(crate) results: Vec<Hit>,
    pub(crate) degraded: Option<String>,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct AccessRow {
    pub(crate) id: String,
    pub(crate) access_count: u32,
    pub(crate) last_accessed_at: Option<String>,
}

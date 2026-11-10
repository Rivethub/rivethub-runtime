use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use config::Config;
use config::Environment;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionArchive;
use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;

use crate::service::StartupError;

const SHARE_DIR: &str = "rivethub/share";

#[derive(Debug, Clone)]
pub(crate) enum Backend {
    Postgres(Arc<PostgresMemory>),
    Sqlite(SqliteStore),
}

#[derive(Debug)]
struct NodeArchive {
    backend: Backend,
    owner: Owner,
    author: BundleAuthor,
}

#[async_trait]
impl SessionArchive for NodeArchive {
    type Session = BundledSession;

    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, MemoryStoreError> {
        Ok(match &self.backend {
            Backend::Postgres(store) => store.session_digests(&self.owner, keys).await?,
            Backend::Sqlite(store) => store.session_digests(&self.owner, keys.to_vec()).await?,
        })
    }

    async fn export(&self, ids: &[Uuid]) -> Result<Vec<BundledSession>, MemoryStoreError> {
        Ok(match &self.backend {
            Backend::Postgres(store) => store.bundled_sessions(&self.author, Some(ids)).await?,
            Backend::Sqlite(store) => {
                store
                    .bundled_sessions(
                        self.author.clone(),
                        Some(ids.iter().map(ToString::to_string).collect()),
                    )
                    .await?
            }
        })
    }
}

pub(crate) fn kit(
    config: &Config,
    env: &Environment,
    owner: Owner,
    backend: Backend,
) -> Result<mcp::sessions::Sessions, StartupError> {
    let node = crate::identity::resolve(config, env)?.name;
    let home = config::Paths::new(env)?.home().to_path_buf();
    let data = match env.text("XDG_DATA_HOME").filter(|text| !text.is_empty()) {
        Some(text) => PathBuf::from(text),
        None => home.join(".local").join("share"),
    };
    let author = BundleAuthor {
        user: owner.as_str().to_owned(),
        node,
    };
    Ok(mcp::sessions::Sessions {
        files: mcp::sessions::ShareFiles::new(data.join(SHARE_DIR), home),
        mapping: config.sanitize_mapping(env)?,
        key: capture::share::key_path(env)?,
        decider: capture::share::decider(config, false),
        redaction: capture::redaction::resolve(Some(config.capture_redaction()), env),
        archive: Arc::new(NodeArchive {
            backend,
            owner: owner.clone(),
            author: author.clone(),
        }),
        owner,
        author,
    })
}

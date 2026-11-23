use std::ffi::OsStr;
use std::path::Path;

use memory_postgres::DatabaseIdentity;
use memory_postgres::PostgresMemory;
use serde::Deserialize;
use serde::Serialize;

use super::Book;
use crate::Error;

pub(super) const TARGET: &str = "target.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum TargetStamp {
    Postgres { cluster: i64, oid: i64 },
    Sqlite { device: u64, inode: u64 },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Recorded {
    Postgres {
        #[serde(default)]
        cluster: Option<i64>,
        #[serde(default)]
        oid: Option<i64>,
    },
    Sqlite {
        device: u64,
        inode: u64,
    },
}

pub(super) async fn verify(backend: &super::Backend) -> Result<TargetStamp, Error> {
    match backend {
        super::Backend::Postgres(url) => postgres(url).await,
        super::Backend::Sqlite(path) => sqlite(path).await,
    }
}

async fn postgres(url: &str) -> Result<TargetStamp, Error> {
    let memory = PostgresMemory::connect_read_only(url).await?;
    let identity = memory.identity().await;
    memory.pool().close().await;
    let identity = identity?;
    Ok(TargetStamp::Postgres {
        cluster: identity.cluster,
        oid: identity.oid,
    })
}

pub(super) fn matches_postgres(stamp: &TargetStamp, identity: &DatabaseIdentity) -> bool {
    match stamp {
        TargetStamp::Postgres { cluster, oid } => {
            *cluster == identity.cluster && *oid == identity.oid
        }
        TargetStamp::Sqlite { .. } => false,
    }
}

pub(super) fn matches_file(stamp: &TargetStamp, device: u64, inode: u64) -> bool {
    match stamp {
        TargetStamp::Sqlite {
            device: found,
            inode: found_inode,
        } => *found == device && *found_inode == inode,
        TargetStamp::Postgres { .. } => false,
    }
}

async fn sqlite(path: &Path) -> Result<TargetStamp, Error> {
    super::super::run::open_sqlite_read_only(path).await?;
    let id = super::propose::file_id(path)?.ok_or(Error::CleanupTargetMissing)?;
    Ok(TargetStamp::Sqlite {
        device: id.device,
        inode: id.inode,
    })
}

pub(super) async fn accept(book: &Book, stamp: &TargetStamp) -> Result<(), Error> {
    match book.read::<Recorded>(TARGET).await? {
        Some(found) if recorded_matches(stamp, &found) => {}
        Some(_) => return Err(Error::CleanupOutMismatch),
        None => {
            if prior_work(book).await? {
                return Err(Error::CleanupOutUnstamped);
            }
            book.write(TARGET, stamp).await?;
        }
    }
    super::propose::refuse_foreign_receipt(book, stamp).await
}

fn recorded_matches(stamp: &TargetStamp, found: &Recorded) -> bool {
    match (stamp, found) {
        (
            TargetStamp::Postgres { cluster, oid },
            Recorded::Postgres {
                cluster: Some(found_cluster),
                oid: Some(found_oid),
            },
        ) => cluster == found_cluster && oid == found_oid,
        (
            TargetStamp::Sqlite { device, inode },
            Recorded::Sqlite {
                device: found_device,
                inode: found_inode,
            },
        ) => device == found_device && inode == found_inode,
        _ => false,
    }
}

async fn prior_work(book: &Book) -> Result<bool, Error> {
    let mut dir = tokio::fs::read_dir(book.directory()).await?;
    while let Some(entry) = dir.next_entry().await? {
        if entry.file_name().as_os_str() != OsStr::new(TARGET) {
            return Ok(true);
        }
    }
    Ok(false)
}

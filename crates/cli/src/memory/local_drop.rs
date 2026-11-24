use std::path::PathBuf;

use capture::seal::DropId;
use config::Environment;
use gateway::drops::Claim;
use gateway::drops::OpenError;

use super::dropbox::DropError;

#[derive(Debug)]
pub(super) enum Local {
    Claimed { file: std::fs::File, bytes: u64 },
    ThroughGateway,
    NotHere,
}

struct DropBoxHere {
    root: PathBuf,
}

async fn drop_box_here(env: &Environment) -> Result<Option<DropBoxHere>, DropError> {
    let loaded = crate::db::load(env, None)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let Some(section) = loaded.config.gateway.as_ref() else {
        return Ok(None);
    };
    if !section.drops.enabled {
        return Ok(None);
    }
    let root = match &section.drops.root {
        Some(root) => root.clone(),
        None => gateway::state_dir(env)
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .join("drops"),
    };
    Ok(Some(DropBoxHere { root }))
}

pub(super) async fn locate(env: &Environment, id: &DropId) -> Result<Local, DropError> {
    let Some(here) = drop_box_here(env).await? else {
        return Ok(Local::NotHere);
    };
    let local_id = id
        .as_str()
        .parse::<gateway::drops::DropId>()
        .map_err(|_| DropError::BadId(id.to_string()))?;
    let root = here.root.clone();
    let claimed = tokio::task::spawn_blocking(move || gateway::drops::claim_in(&root, &local_id))
        .await
        .map_err(std::io::Error::other)?;
    match claimed {
        Ok(Claim::Ready { file, bytes }) => Ok(Local::Claimed { file, bytes }),
        Ok(Claim::Gone) => Ok(Local::NotHere),
        Ok(Claim::Failed) => Err(DropError::Io(std::io::Error::other(
            "this node's drop box failed while reading the bundle; ask the sender to share it again",
        ))),
        Err(OpenError::Unsafe { .. } | OpenError::Io { .. }) => Ok(Local::ThroughGateway),
    }
}

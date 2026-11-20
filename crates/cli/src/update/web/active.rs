use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use super::{has_index, installed};
use crate::update::UpdateError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub path: PathBuf,
    pub build: Option<Build>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: Version,
    pub min_rh_version: Version,
}

pub fn active(state: &Path) -> Result<Option<Active>, UpdateError> {
    let path = gateway::managed_web_link(state);
    if !has_index(&path)? {
        return Ok(None);
    }
    let build = match installed(&path) {
        Ok(manifest) => Some(Build {
            version: manifest.version,
            min_rh_version: manifest.min_rh_version,
        }),
        Err(UpdateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(Some(Active { path, build }))
}

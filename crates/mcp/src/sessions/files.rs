use std::os::unix::fs::DirBuilderExt as _;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use capture::share::LOCAL_SUFFIX;
use capture::share::REPORT_SUFFIX;
use serde::Deserialize;
use thiserror::Error;

const NAME_MAX: usize = 100;
const BUNDLE_EXTENSION: &str = "json";
const MAPPING_EXTENSION: &str = "toml";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct NameError(pub(crate) String);

#[derive(Debug, Error)]
pub enum PathRefusal {
    #[error("refused: {0} does not exist")]
    Missing(PathBuf),
    #[error("refused: {0} is outside your home directory")]
    OutsideHome(PathBuf),
    #[error("refused: {0} is inside a hidden directory")]
    Hidden(PathBuf),
    #[error("refused: {0} is not a regular file")]
    NotAFile(PathBuf),
    #[error("refused: {path} is not a .{wanted} file")]
    WrongKind { path: PathBuf, wanted: &'static str },
    #[error(
        "refused: {0} already exists; session tools never replace a file, so choose another out name"
    )]
    Exists(PathBuf),
    #[error("cannot create the share directory {path}: {source}")]
    Share {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct BundleName(String);

impl BundleName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BundleName {
    type Error = NameError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let refused = || {
            NameError(format!(
                "out {name:?} is refused: name a file in the share directory with letters, digits, '.', '_' and '-', starting with a letter or digit and ending in .json (not {REPORT_SUFFIX} or {LOCAL_SUFFIX})"
            ))
        };
        let stem = name.strip_suffix(".json").ok_or_else(refused)?;
        if !bare(stem) || name.ends_with(LOCAL_SUFFIX) {
            return Err(refused());
        }
        Ok(Self(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum ReadPath {
    Shared(String),
    Home(PathBuf),
}

impl TryFrom<String> for ReadPath {
    type Error = NameError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if bare(&text) {
            return Ok(Self::Shared(text));
        }
        let path = PathBuf::from(&text);
        if path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Ok(Self::Home(path));
        }
        Err(NameError(format!(
            "{text:?} is refused: name a file in the share directory, or give an absolute path under your home directory"
        )))
    }
}

fn bare(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && name.len() <= NAME_MAX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Bundle,
    Mapping,
}

impl Kind {
    const fn extension(self) -> &'static str {
        match self {
            Self::Bundle => BUNDLE_EXTENSION,
            Self::Mapping => MAPPING_EXTENSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareFiles {
    share: PathBuf,
    home: PathBuf,
}

impl ShareFiles {
    pub const fn new(share: PathBuf, home: PathBuf) -> Self {
        Self { share, home }
    }

    pub(crate) async fn target(&self, name: &BundleName) -> Result<PathBuf, PathRefusal> {
        let share = self.share.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&share)
                .map_err(|source| PathRefusal::Share {
                    path: share.clone(),
                    source,
                })
        })
        .await
        .map_err(|source| PathRefusal::Share {
            path: self.share.clone(),
            source: std::io::Error::other(source),
        })??;
        Ok(self.share.join(name.as_str()))
    }

    pub(crate) async fn readable(
        &self,
        source: &ReadPath,
        kind: Kind,
    ) -> Result<PathBuf, PathRefusal> {
        let named = match source {
            ReadPath::Shared(name) => self.share.join(name),
            ReadPath::Home(path) => path.clone(),
        };
        let path = tokio::fs::canonicalize(&named)
            .await
            .map_err(|_| PathRefusal::Missing(named.clone()))?;
        let home = tokio::fs::canonicalize(&self.home)
            .await
            .map_err(|_| PathRefusal::OutsideHome(path.clone()))?;
        let inside = path
            .strip_prefix(&home)
            .map_err(|_| PathRefusal::OutsideHome(path.clone()))?;
        let shared = tokio::fs::canonicalize(&self.share)
            .await
            .is_ok_and(|share| path.starts_with(share));
        if !shared && inside.components().any(hidden) {
            return Err(PathRefusal::Hidden(path));
        }
        if !tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Err(PathRefusal::NotAFile(path));
        }
        if path
            .extension()
            .is_none_or(|extension| extension != kind.extension())
        {
            return Err(PathRefusal::WrongKind {
                path,
                wanted: kind.extension(),
            });
        }
        Ok(path)
    }
}

fn hidden(part: Component<'_>) -> bool {
    match part {
        Component::Normal(name) => name.as_encoded_bytes().first() == Some(&b'.'),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    }
}

pub(crate) async fn refuse_existing(paths: &[&Path]) -> Result<(), PathRefusal> {
    for path in paths {
        if tokio::fs::symlink_metadata(path).await.is_ok() {
            return Err(PathRefusal::Exists(path.to_path_buf()));
        }
    }
    Ok(())
}

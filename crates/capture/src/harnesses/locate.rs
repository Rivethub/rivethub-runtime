use std::path::Path;
use std::path::PathBuf;

use config::Environment;

use super::registry::Base;
use super::registry::Location;
use crate::listing::StoreRoots;
use crate::listing::claude_config_file;
use crate::listing::store_roots;

#[derive(Debug, Clone)]
pub struct Locator {
    home: Option<PathBuf>,
    xdg_config: Option<PathBuf>,
    claude_json_dir: Option<PathBuf>,
    roots: StoreRoots,
    path_dirs: Vec<PathBuf>,
}

impl Locator {
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        let home = env
            .home()
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let xdg_config = env
            .text("XDG_CONFIG_HOME")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".config")));
        let claude_json_dir =
            claude_config_file(env).and_then(|file| file.parent().map(Path::to_path_buf));
        let path_dirs = env
            .get("PATH")
            .map(|paths| std::env::split_paths(paths).collect())
            .unwrap_or_default();
        Self {
            home,
            xdg_config,
            claude_json_dir,
            roots: store_roots(env),
            path_dirs,
        }
    }

    fn bases(&self, base: Base) -> Vec<PathBuf> {
        let single = |path: Option<&PathBuf>| path.cloned().into_iter().collect();
        match base {
            Base::Home => single(self.home.as_ref()),
            Base::XdgConfig => single(self.xdg_config.as_ref()),
            Base::OpencodeConfig => self
                .xdg_config
                .iter()
                .map(|config| config.join("opencode"))
                .collect(),
            Base::ClaudeDir => single(self.roots.claude.as_ref()),
            Base::ClaudeJsonDir => single(self.claude_json_dir.as_ref()),
            Base::Grok => single(self.roots.grok.as_ref()),
            Base::Hermes => single(self.roots.hermes.as_ref()),
            Base::Kimi => single(self.roots.kimi.as_ref()),
            Base::Codex => single(self.roots.codex.as_ref()),
            Base::OpencodeData => single(self.roots.opencode.as_ref()),
            Base::Omp => single(self.roots.omp.as_ref()),
            Base::Cursor => single(self.roots.cursor.as_ref()),
            Base::Dsh => single(self.roots.dsh.as_ref()),
            Base::Cowork => self.roots.cowork.clone(),
        }
    }

    #[must_use]
    pub fn candidates(&self, location: Location) -> Vec<PathBuf> {
        self.bases(location.base)
            .into_iter()
            .map(|base| {
                if location.relative.is_empty() {
                    base
                } else {
                    base.join(location.relative)
                }
            })
            .collect()
    }

    #[must_use]
    pub fn existing(&self, location: Location) -> Option<PathBuf> {
        self.candidates(location)
            .into_iter()
            .find(|path| path.exists())
    }

    #[must_use]
    pub fn target(&self, location: Location) -> Option<PathBuf> {
        let candidates = self.candidates(location);
        candidates
            .iter()
            .find(|path| path.exists())
            .or_else(|| candidates.first())
            .cloned()
    }

    #[must_use]
    pub fn which(&self, binary: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    }
}

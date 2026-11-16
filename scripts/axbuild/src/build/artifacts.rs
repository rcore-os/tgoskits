//! Generated host files are described during resolution and written only for execution.
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub(crate) struct BuildFiles {
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, String, bool)>,
}
impl BuildFiles {
    pub(super) fn directory(&mut self, path: PathBuf) -> PathBuf {
        self.directories.push(path.clone());
        path
    }
    pub(super) fn file(&mut self, path: PathBuf, contents: String, executable: bool) -> PathBuf {
        self.files.push((path.clone(), contents, executable));
        path
    }
    pub(crate) fn contents(&self, path: &Path) -> Option<&str> {
        self.files
            .iter()
            .find(|(p, ..)| p == path)
            .map(|(_, s, _)| s.as_str())
    }
    pub(crate) fn materialize(&self) -> anyhow::Result<()> {
        for dir in &self.directories {
            fs::create_dir_all(dir)?;
        }
        for (path, contents, executable) in &self.files {
            super::std_build::write_if_changed(path, contents)?;
            if *executable {
                super::std_build::set_executable(path)?;
            }
        }
        Ok(())
    }
}

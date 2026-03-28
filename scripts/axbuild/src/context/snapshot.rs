use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};

pub(crate) trait CommandSnapshotFile: Default + Serialize + DeserializeOwned {
    const FILE_NAME: &'static str;
}

pub(crate) fn load_snapshot<S>(root: &Path) -> anyhow::Result<S>
where
    S: CommandSnapshotFile,
{
    let path = root.join(S::FILE_NAME);
    if !path.exists() {
        return Ok(S::default());
    }

    toml::from_str(&std::fs::read_to_string(&path)?)
        .with_context(|| format!("failed to parse snapshot {}", path.display()))
}

pub(crate) fn store_snapshot<S>(root: &Path, snapshot: &S) -> anyhow::Result<PathBuf>
where
    S: CommandSnapshotFile,
{
    let path = root.join(S::FILE_NAME);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(snapshot)?)?;
    Ok(path)
}

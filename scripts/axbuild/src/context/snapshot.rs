use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::{Serialize, de::DeserializeOwned};
use tempfile::NamedTempFile;

pub(crate) trait CommandSnapshotFile: Default + Serialize + DeserializeOwned {
    const FILE_NAME: &'static str;
}

pub(crate) fn load_snapshot<S>(root: &Path) -> anyhow::Result<S>
where
    S: CommandSnapshotFile,
{
    let path = snapshot_path::<S>(root);
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
    let path = snapshot_path::<S>(root);
    let contents = toml::to_string_pretty(snapshot)?;
    replace_file(stage_file(&path, &contents)?, &path)?;
    Ok(path)
}

/// Publish an expanded configuration and its selection, restoring the previous
/// configuration if snapshot publication fails. This is not crash-atomic and
/// requires exclusive access to these paths for the duration of the command.
pub(crate) fn export_config_with_snapshot<C, S>(
    root: &Path,
    config_path: &Path,
    config: &C,
    snapshot: &S,
) -> anyhow::Result<()>
where
    C: Serialize,
    S: CommandSnapshotFile,
{
    PreparedDefconfig::new(config_path, config, &snapshot_path::<S>(root), snapshot)?.commit()
}

pub(crate) fn snapshot_path<S>(root: &Path) -> PathBuf
where
    S: CommandSnapshotFile,
{
    crate::context::axbuild_tmp_dir(root).join(S::FILE_NAME)
}

struct PreparedDefconfig {
    config_path: PathBuf,
    config_file: NamedTempFile,
    snapshot_path: PathBuf,
    snapshot_file: NamedTempFile,
    previous_config: Option<NamedTempFile>,
}

impl PreparedDefconfig {
    fn new(
        config_path: &Path,
        config: &impl Serialize,
        snapshot_path: &Path,
        snapshot: &impl Serialize,
    ) -> anyhow::Result<Self> {
        // Both serializers can fail; neither destination has been touched yet.
        let config_contents = toml::to_string_pretty(config)?;
        let snapshot_contents = toml::to_string_pretty(snapshot)?;
        let config_file = stage_file(config_path, &config_contents)?;
        let snapshot_file = stage_file(snapshot_path, &snapshot_contents)?;
        let previous_config = match std::fs::symlink_metadata(config_path) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    bail!(
                        "configuration destination is not a regular file: {}",
                        config_path.display()
                    );
                }
                let backup = NamedTempFile::new_in(parent_dir(config_path))?;
                std::fs::copy(config_path, backup.path())?;
                backup.as_file().sync_all()?;
                Some(backup)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            config_path: config_path.to_owned(),
            config_file,
            snapshot_path: snapshot_path.to_owned(),
            snapshot_file,
            previous_config,
        })
    }

    fn commit(self) -> anyhow::Result<()> {
        replace_file(self.config_file, &self.config_path)?;
        if let Err(commit_error) = replace_file(self.snapshot_file, &self.snapshot_path) {
            if let Err(rollback_error) = restore_config(&self.config_path, self.previous_config) {
                bail!("{commit_error:#}; configuration rollback also failed: {rollback_error:#}");
            }
            return Err(
                commit_error.context("configuration restored after snapshot publication failed")
            );
        }
        Ok(())
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn stage_file(path: &Path, contents: &str) -> anyhow::Result<NamedTempFile> {
    let parent = parent_dir(path);
    std::fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to stage {}", path.display()))?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

fn replace_file(temporary: NamedTempFile, path: &Path) -> anyhow::Result<()> {
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn restore_config(path: &Path, previous: Option<NamedTempFile>) -> anyhow::Result<()> {
    if let Some(previous) = previous {
        // Disarm automatic deletion before attempting recovery, so a second
        // I/O error leaves the original bytes available for manual recovery.
        let recovery_path = previous.into_temp_path().keep()?;
        std::fs::rename(&recovery_path, path).with_context(|| {
            format!(
                "failed to restore {}; original configuration retained at {}",
                path.display(),
                recovery_path.display()
            )
        })?;
    } else {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove new configuration {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ArceosCommandSnapshot;

    #[test]
    fn defconfig_restores_configuration_and_selection_after_snapshot_commit_failure() {
        for existing_config in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            if existing_config {
                std::fs::write(&config_path, "user configuration").unwrap();
            }
            let previous = ArceosCommandSnapshot {
                config: Some(config_path.clone()),
                ..Default::default()
            };
            let selection_path = store_snapshot(root.path(), &previous).unwrap();
            let selection_bytes = std::fs::read(&selection_path).unwrap();
            let next = ArceosCommandSnapshot {
                preset: Some("synthetic-preset".into()),
                ..Default::default()
            };
            let prepared =
                PreparedDefconfig::new(&config_path, &next, &selection_path, &next).unwrap();
            // Inject a failure after staging without changing either old file.
            // The real commit must undo the configuration it publishes first.
            std::fs::remove_file(prepared.snapshot_file.path()).unwrap();
            assert!(prepared.commit().is_err());
            assert_eq!(std::fs::read(&selection_path).unwrap(), selection_bytes);
            assert_eq!(
                load_snapshot::<ArceosCommandSnapshot>(root.path()).unwrap(),
                previous
            );
            if existing_config {
                assert_eq!(std::fs::read(&config_path).unwrap(), b"user configuration");
            } else {
                assert!(!config_path.exists());
            }
        }
    }

    #[test]
    fn failed_configuration_restore_retains_recovery_copy() {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        std::fs::create_dir(&config_path).unwrap();
        let mut previous = NamedTempFile::new_in(root.path()).unwrap();
        previous.write_all(b"original configuration").unwrap();
        let recovery_path = previous.path().to_owned();
        let error = restore_config(&config_path, Some(previous)).unwrap_err();
        assert!(format!("{error:#}").contains(&recovery_path.display().to_string()));
        assert_eq!(
            std::fs::read(&recovery_path).unwrap(),
            b"original configuration"
        );
    }
}

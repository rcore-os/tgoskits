use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use ax_config::{CommonConfig, ConfigError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) mod presets;

/// Final StarryOS input; its Cargo package is fixed by the adapter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[serde(default)]
pub struct StarryConfig {
    #[serde(flatten)]
    pub common: CommonConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_its: Option<PathBuf>,
    #[serde(flatten, skip_serializing)]
    #[schemars(skip)]
    extra: toml::Table,
}

impl StarryConfig {
    /// Validate a fully selected configuration without reading process state.
    pub fn validate(&self) -> Result<(), ConfigError> {
        <Self as crate::build::BuildConfiguration>::validate_input(self)?;
        crate::build::validate_config_target(&self.common)
    }

    /// Generate the production input contract, including overlay controls.
    pub fn input_schema() -> Result<schemars::Schema, ConfigError> {
        ax_config::input_schema::<Self>()
    }

    pub(crate) fn validate_resources(&self, workspace_root: &Path) -> anyhow::Result<()> {
        if let Some(path) = &self.fit_its {
            let path = workspace_root.join(path);
            let file = fs::File::open(&path)
                .map_err(|error| anyhow!("failed to read fit_its {}: {error}", path.display()))?;
            if !file.metadata()?.is_file() {
                anyhow::bail!("fit_its {} is not a file", path.display());
            }
        }
        Ok(())
    }
}

impl crate::build::BuildConfiguration for StarryConfig {
    const CLEARABLE_FIELDS: &'static [&'static str] = &["fit_its"];

    fn preset(name: &str) -> Option<Self> {
        presets::find_preset(name).map(|preset| preset.materialize())
    }

    fn default_qemu_preset(target: &str) -> Option<&'static str> {
        presets::default_qemu_preset(target).map(|preset| preset.name)
    }

    fn common(&self) -> &CommonConfig {
        &self.common
    }

    fn check_source(&self, overlay: &ax_config::Overlay) -> anyhow::Result<()> {
        if self.fit_its.is_none()
            && !overlay
                .controls
                .unset
                .iter()
                .any(|field| field == "fit_its")
            && overlay.source().with_extension("its").exists()
        {
            anyhow::bail!(
                "build config {} has an ambiguous companion ITS; set `fit_its` explicitly \
                 (relative to the workspace root), select a preset, or use `unset = ['fit_its']`",
                overlay.source().display()
            );
        }
        Ok(())
    }

    fn extra_fields(&self) -> &toml::Table {
        &self.extra
    }

    fn validate_input(&self) -> Result<(), ConfigError> {
        crate::build::reject_extra_fields(&self.extra)?;
        self.common.validate()?;
        if let Some(path) = &self.fit_its {
            crate::build::validate_resource_path("fit_its", path)?;
        }
        Ok(())
    }
}

/// Export a fully expanded recipe, then select the written file for later commands.
pub(crate) fn write_defconfig(workspace_root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    use crate::context::{
        STARRY_PACKAGE, StarryCommandSnapshot, snapshot_path_value, starry_arch_for_target_checked,
    };

    let preset = presets::find_preset(name).ok_or_else(|| ConfigError::PresetNotFound {
        origin: None,
        name: name.to_owned(),
    })?;
    let config = preset.materialize();
    config.validate()?;
    config.validate_resources(workspace_root)?;
    let target = preset.common.board.target;
    let path =
        crate::build::default_build_info_path_in_workspace(workspace_root, STARRY_PACKAGE, target);
    let snapshot = StarryCommandSnapshot {
        arch: Some(starry_arch_for_target_checked(target)?.to_owned()),
        target: Some(target.to_owned()),
        config: Some(snapshot_path_value(workspace_root, &path)),
        ..Default::default()
    };
    crate::context::export_config_with_snapshot(workspace_root, &path, &config, &snapshot)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        DEFAULT_STARRY_TARGET, STARRY_SNAPSHOT_FILE, StarryCommandSnapshot, StarryQemuSnapshot,
    };

    #[test]
    fn write_defconfig_roundtrips_selected_input_and_preserves_snapshot_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let preset = presets::default_qemu_preset(DEFAULT_STARRY_TARGET).unwrap();
        let previous = StarryCommandSnapshot {
            smp: Some(7),
            qemu: StarryQemuSnapshot {
                qemu_config: Some("old-qemu.toml".into()),
            },
            ..Default::default()
        };
        previous.store(root.path()).unwrap();
        let path = write_defconfig(root.path(), preset.name).unwrap();
        let exported: StarryConfig = crate::build::load_build_config(&path).unwrap();
        assert_eq!(exported, preset.materialize());
        let selected = StarryCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(
            selected.config,
            Some(crate::context::snapshot_path_value(root.path(), &path))
        );
        assert_eq!(selected.smp, None);
        assert_eq!(selected.qemu.qemu_config, None);
        assert_eq!(selected.uboot.uboot_config, None);
        assert_eq!(selected.preset, None);
        let snapshot_path = crate::context::axbuild_tmp_dir(root.path()).join(STARRY_SNAPSHOT_FILE);
        let saved_snapshot = fs::read(&snapshot_path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("unchanged"), "sentinel").unwrap();
        assert!(write_defconfig(root.path(), preset.name).is_err());
        assert_eq!(fs::read(&snapshot_path).unwrap(), saved_snapshot);
        assert_eq!(
            fs::read_to_string(path.join("unchanged")).unwrap(),
            "sentinel"
        );
        assert!(write_defconfig(root.path(), "unknown-synthetic-preset").is_err());
        assert_eq!(fs::read(snapshot_path).unwrap(), saved_snapshot);
    }
}

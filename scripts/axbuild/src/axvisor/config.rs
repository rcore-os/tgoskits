use std::path::PathBuf;

use ax_config::{CommonConfig, ConfigError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) mod presets;

/// Final Axvisor input. VM files remain packaging inputs, not Cargo identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[serde(default)]
pub struct AxvisorConfig {
    #[serde(flatten)]
    pub common: CommonConfig,
    pub vm_configs: Vec<PathBuf>,
    #[serde(flatten, skip_serializing)]
    #[schemars(skip)]
    extra: toml::Table,
}

impl AxvisorConfig {
    /// Validate a fully selected configuration without reading process state.
    pub fn validate(&self) -> Result<(), ConfigError> {
        <Self as crate::build::BuildConfiguration>::validate_input(self)?;
        crate::build::validate_config_target(&self.common)
    }

    /// Generate the production input contract, including overlay controls.
    pub fn input_schema() -> Result<schemars::Schema, ConfigError> {
        ax_config::input_schema::<Self>()
    }
}

impl crate::build::BuildConfiguration for AxvisorConfig {
    const CLEARABLE_FIELDS: &'static [&'static str] = &[];

    fn preset(name: &str) -> Option<Self> {
        presets::find_preset(name).map(|preset| preset.materialize())
    }

    fn default_qemu_preset(target: &str) -> Option<&'static str> {
        presets::default_qemu_preset(target).map(|preset| preset.name)
    }

    fn common(&self) -> &CommonConfig {
        &self.common
    }

    fn extra_fields(&self) -> &toml::Table {
        &self.extra
    }

    fn validate_input(&self) -> Result<(), ConfigError> {
        crate::build::reject_extra_fields(&self.extra)?;
        self.common.validate()?;
        super::build::reject_unsupported_nested_platform_features(&self.common.features, &[])?;
        for path in &self.vm_configs {
            crate::build::validate_resource_path("vm_configs", path)?;
        }
        Ok(())
    }
}

/// Export one fully expanded built-in recipe to Axvisor's target-specific
/// default config, then update the snapshot only after the atomic file replace.
pub(crate) fn write_defconfig(
    workspace_root: &std::path::Path,
    name: &str,
) -> anyhow::Result<PathBuf> {
    let input = crate::build::load_config_input::<AxvisorConfig>(workspace_root, None, Some(name))?;
    input.config.validate()?;
    crate::build::validate_features(&input.config.common)?;
    let target = input
        .config
        .common
        .target
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Axvisor preset `{name}` has no target"))?;
    let arch = crate::context::arch_for_target_checked(target)?;
    let path = crate::build::default_build_info_path_in_workspace(
        workspace_root,
        crate::axvisor::build::AXVISOR_PACKAGE,
        target,
    );
    let previous_snapshot = crate::context::AxvisorCommandSnapshot::load(workspace_root)?;

    crate::build::export_config(&path, &input.config)?;

    let mut snapshot = previous_snapshot;
    snapshot.preset = None;
    snapshot.config = Some(crate::context::snapshot_path_value(workspace_root, &path));
    snapshot.arch = Some(arch.to_owned());
    snapshot.target = Some(target.to_owned());
    snapshot.smp = None;
    snapshot.vmconfigs.clear();
    snapshot.qemu.qemu_config = None;
    snapshot.uboot.uboot_config = None;
    snapshot.store(workspace_root)?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::*;
    use crate::context::{AxvisorCommandSnapshot, AxvisorQemuSnapshot, AxvisorUbootSnapshot};

    #[test]
    fn defconfig_exports_expanded_preset_then_clears_old_snapshot_selectors() {
        let root = tempdir().unwrap();
        AxvisorCommandSnapshot {
            preset: Some("removed-board-template".into()),
            arch: Some("x86_64".into()),
            target: Some("x86_64-unknown-none".into()),
            smp: Some(4),
            config: Some(PathBuf::from("old.toml")),
            vmconfigs: vec![PathBuf::from("old-vm.toml")],
            qemu: AxvisorQemuSnapshot {
                qemu_config: Some(PathBuf::from("old-qemu.toml")),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: Some(PathBuf::from("old-uboot.toml")),
            },
        }
        .store(root.path())
        .unwrap();

        let path = write_defconfig(root.path(), "qemu-aarch64").unwrap();
        let exported: AxvisorConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            exported.common.target.as_deref(),
            Some(ax_config::boards::TARGET_AARCH64)
        );
        assert_eq!(exported.vm_configs, Vec::<PathBuf>::new());

        let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.preset, None);
        assert_eq!(
            snapshot.config,
            Some(crate::context::snapshot_path_value(root.path(), &path))
        );
        assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some(ax_config::boards::TARGET_AARCH64)
        );
        assert_eq!(snapshot.smp, None);
        assert!(snapshot.vmconfigs.is_empty());
        assert_eq!(snapshot.qemu.qemu_config, None);
        assert_eq!(snapshot.uboot.uboot_config, None);
    }
}

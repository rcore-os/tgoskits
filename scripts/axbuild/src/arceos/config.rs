use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use ax_config::{CommonConfig, ConfigError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) mod presets;

/// Final ArceOS input. Unknown root fields are retained only for diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[serde(default)]
pub struct ArceosConfig {
    #[serde(flatten)]
    pub common: CommonConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub to_bin: bool,
    pub freestanding: bool,
    #[serde(rename = "app-c", skip_serializing_if = "Option::is_none")]
    pub app_c: Option<PathBuf>,
    #[serde(flatten, skip_serializing)]
    #[schemars(skip)]
    extra: toml::Table,
}

impl ArceosConfig {
    /// Validate a fully selected configuration without reading process state.
    pub fn validate(&self) -> Result<(), ConfigError> {
        <Self as crate::build::BuildConfiguration>::validate_input(self)?;
        crate::build::validate_config_target(&self.common)?;
        if self.app_c.is_none() && self.package.as_deref().is_none_or(str::is_empty) {
            return Err(crate::build::invalid_config_value(
                "package",
                "<unspecified>",
                "an ArceOS application package",
            ));
        }
        Ok(())
    }

    /// Generate the same input contract used by the production overlay parser.
    pub fn input_schema() -> Result<schemars::Schema, ConfigError> {
        ax_config::input_schema::<Self>()
    }
}

impl crate::build::BuildConfiguration for ArceosConfig {
    const CLEARABLE_FIELDS: &'static [&'static str] = &["app-c"];

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
        if self.freestanding && self.app_c.is_some() {
            return Err(crate::build::invalid_config_value(
                "app-c",
                "<present>",
                "freestanding Rust and app-c are distinct build inputs",
            ));
        }
        if let Some(app_c) = &self.app_c {
            crate::build::validate_resource_path("app-c", app_c)?;
            if let Some(package) = &self.package
                && package != super::cbuild::AX_LIBC_PACKAGE
            {
                return Err(crate::build::invalid_config_value(
                    "package",
                    package,
                    "ax-libc when app-c is selected",
                ));
            }
        }
        Ok(())
    }
}

use crate::context::{
    ArceosCommandSnapshot, ArceosQemuSnapshot, ArceosUbootSnapshot, arch_for_target_checked,
    snapshot_path_value,
};

pub(crate) fn write_defconfig(workspace_root: &Path, preset_name: &str) -> anyhow::Result<PathBuf> {
    let preset = presets::find_preset(preset_name).ok_or_else(|| {
        anyhow!(
            "unknown ArceOS preset `{preset_name}`; available presets: {}",
            presets::PRESETS
                .iter()
                .map(|preset| preset.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let mut config = preset.materialize();
    if let Some(app_c) = config.app_c.as_deref() {
        let app_c = std::path::absolute(workspace_root.join(app_c))?;
        config.app_c = Some(super::build::resolve_app_c_dir(&app_c)?);
    }
    config.validate()?;
    let package = config
        .package
        .as_deref()
        .context("ArceOS preset must specify a package")?;
    let target = config
        .common
        .target
        .as_deref()
        .context("ArceOS preset must specify a target")?;
    let build_config_path =
        crate::build::default_build_info_path_in_workspace(workspace_root, package, target);
    crate::build::export_config(&build_config_path, &config)?;
    let snapshot = ArceosCommandSnapshot {
        preset: None,
        package: Some(package.to_owned()),
        arch: Some(arch_for_target_checked(target)?.to_string()),
        target: Some(target.to_owned()),
        smp: None,
        config: Some(snapshot_path_value(workspace_root, &build_config_path)),
        qemu: ArceosQemuSnapshot::default(),
        uboot: ArceosUbootSnapshot::default(),
    };
    snapshot.store(workspace_root)?;
    Ok(build_config_path)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn write_defconfig_exports_materialized_preset_and_replaces_snapshot_selection() {
        let root = tempdir().unwrap();
        let existing_snapshot = ArceosCommandSnapshot {
            preset: Some("old-preset".to_string()),
            package: Some("old-package".to_string()),
            arch: Some("riscv64".to_string()),
            target: Some("riscv64gc-unknown-none-elf".to_string()),
            smp: Some(4),
            config: None,
            qemu: ArceosQemuSnapshot {
                qemu_config: Some("configs/qemu.toml".into()),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: Some("configs/uboot.toml".into()),
            },
        };
        existing_snapshot.store(root.path()).unwrap();

        let path = write_defconfig(root.path(), "orangepi-5-plus").unwrap();

        assert_eq!(
            path,
            root.path().join(
                "tmp/axbuild/config/arceos-helloworld/build-aarch64-unknown-none-softfloat.toml"
            )
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("package = \"arceos-helloworld\""));
        assert!(written.contains("target = \"aarch64-unknown-none-softfloat\""));
        assert!(written.contains("max_cpu_num = 8"));
        assert!(!written.contains("axconfig"));

        let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.preset, None);
        assert_eq!(snapshot.package.as_deref(), Some("arceos-helloworld"));
        assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some("aarch64-unknown-none-softfloat")
        );
        assert_eq!(snapshot.smp, None);
        assert_eq!(
            snapshot.config,
            Some(
                "tmp/axbuild/config/arceos-helloworld/build-aarch64-unknown-none-softfloat.toml"
                    .into()
            )
        );
        assert_eq!(snapshot.qemu.qemu_config, None);
        assert_eq!(snapshot.uboot.uboot_config, None);

        // A snapshot destination that cannot be replaced must not publish a
        // new configuration, whether the configuration already exists or not.
        let snapshot_path = root
            .path()
            .join("tmp/axbuild")
            .join(crate::context::ARCEOS_SNAPSHOT_FILE);
        std::fs::remove_file(&snapshot_path).unwrap();
        std::fs::create_dir(&snapshot_path).unwrap();
        std::fs::write(snapshot_path.join("unchanged"), "old selection").unwrap();
        std::fs::write(&path, "user configuration").unwrap();
        assert!(write_defconfig(root.path(), "orangepi-5-plus").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"user configuration");
        std::fs::remove_file(&path).unwrap();
        assert!(write_defconfig(root.path(), "orangepi-5-plus").is_err());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(snapshot_path.join("unchanged")).unwrap(),
            b"old selection"
        );
    }

    #[test]
    fn write_defconfig_reports_available_presets_for_unknown_name() {
        let root = tempdir().unwrap();
        let err = write_defconfig(root.path(), "missing")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown ArceOS preset `missing`"));
        assert!(err.contains("orangepi-5-plus"));
    }
}

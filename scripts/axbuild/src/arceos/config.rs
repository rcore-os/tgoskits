use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
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

use super::board::{self, Board};
use crate::context::{
    ArceosCommandSnapshot, ArceosQemuSnapshot, ArceosUbootSnapshot, arch_for_target_checked,
    snapshot_path_value,
};

pub(crate) fn available_board_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    board::board_names(workspace_root)
}

fn resolve_board(workspace_root: &Path, name: &str) -> anyhow::Result<Board> {
    board::find_board(workspace_root, name)?.ok_or_else(|| {
        let available = available_board_names(workspace_root).unwrap_or_default();
        anyhow!(
            "unknown ArceOS board `{name}` in {}; available boards: {}",
            board::board_dir(workspace_root)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "os/arceos/configs/board".to_string()),
            available.join(", ")
        )
    })
}

fn write_board_to_default_build_config(
    workspace_root: &Path,
    board: &Board,
) -> anyhow::Result<PathBuf> {
    let build_config_path = crate::build::default_build_info_path_in_workspace(
        workspace_root,
        &board.package,
        &board.target,
    );
    write_board_to_default_build_config_at(&build_config_path, board)?;
    Ok(build_config_path)
}

fn update_snapshot_for_board(
    workspace_root: &Path,
    board: &Board,
    build_config_path: &Path,
) -> anyhow::Result<()> {
    let snapshot = ArceosCommandSnapshot {
        package: Some(board.package.clone()),
        arch: Some(arch_for_target_checked(&board.target)?.to_string()),
        target: Some(board.target.clone()),
        smp: board.build_config.common.max_cpu_num,
        config: Some(snapshot_path_value(workspace_root, build_config_path)),
        qemu: ArceosQemuSnapshot::default(),
        uboot: ArceosUbootSnapshot::default(),
    };
    snapshot.store(workspace_root)?;
    Ok(())
}

pub(crate) fn ensure_default_build_config_for_target(
    workspace_root: &Path,
    package: &str,
    target: &str,
    build_config_path: &Path,
) -> anyhow::Result<Option<Board>> {
    if build_config_path.exists() {
        return Ok(None);
    }

    let Some(board) = board::default_qemu_board(workspace_root, target, Some(package))? else {
        return Ok(None);
    };
    // This only materializes a missing build config. The command dispatcher owns
    // snapshot persistence, so implicit config creation cannot alter it here.
    write_board_to_default_build_config_at(build_config_path, &board)?;
    Ok(Some(board))
}

pub(crate) fn write_defconfig(workspace_root: &Path, board_name: &str) -> anyhow::Result<PathBuf> {
    let board = resolve_board(workspace_root, board_name)?;
    let build_config_path = write_board_to_default_build_config(workspace_root, &board)?;
    update_snapshot_for_board(workspace_root, &board, &build_config_path)?;
    Ok(build_config_path)
}

fn write_board_to_default_build_config_at(
    build_config_path: &Path,
    board: &Board,
) -> anyhow::Result<()> {
    if let Some(parent) = build_config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&board.path, build_config_path).map_err(|e| {
        anyhow!(
            "failed to copy ArceOS board config {} to {}: {e}",
            board.path.display(),
            build_config_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;
    use crate::context::{ArceosCommandSnapshot, ArceosQemuSnapshot, ArceosUbootSnapshot};

    fn write_workspace(root: &Path) {
        fs::create_dir_all(root.join("os/arceos")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"apps/arceos/helloworld\"]\n",
        )
        .unwrap();
    }

    fn write_board(root: &Path, name: &str, body: &str) {
        let path = crate::arceos::board::board_dir(root)
            .unwrap()
            .join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn write_defconfig_generates_dynamic_build_toml_and_updates_snapshot() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "orangepi-5-plus",
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
max_cpu_num = 1
"#,
        );
        let existing_snapshot = ArceosCommandSnapshot {
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
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("package = \"arceos-helloworld\""));
        assert!(written.contains("target = \"aarch64-unknown-none-softfloat\""));
        assert!(!written.contains("axconfig"));

        let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.package.as_deref(), Some("arceos-helloworld"));
        assert_eq!(snapshot.arch.as_deref(), Some("aarch64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some("aarch64-unknown-none-softfloat")
        );
        assert_eq!(snapshot.smp, Some(1));
        assert_eq!(
            snapshot.config,
            Some(
                "tmp/axbuild/config/arceos-helloworld/build-aarch64-unknown-none-softfloat.toml"
                    .into()
            )
        );
        assert_eq!(snapshot.qemu.qemu_config, None);
        assert_eq!(snapshot.uboot.uboot_config, None);
    }

    #[test]
    fn write_defconfig_reports_board_directory_for_unknown_name() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "orangepi-5-plus",
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
"#,
        );

        let err = write_defconfig(root.path(), "missing")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown ArceOS board `missing`"));
        assert!(err.contains("orangepi-5-plus"));
    }
}

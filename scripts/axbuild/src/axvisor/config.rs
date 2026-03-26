use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    axvisor::{board, build::AxvisorBoardConfig},
    context::AxvisorCommandSnapshot,
};

pub const DEFAULT_BUILD_CONFIG_RELATIVE_PATH: &str = "os/axvisor/.build.toml";

pub fn default_build_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(DEFAULT_BUILD_CONFIG_RELATIVE_PATH)
}

pub fn available_board_names() -> Vec<&'static str> {
    board::board_names()
}

pub fn resolve_board_config(name: &str) -> anyhow::Result<AxvisorBoardConfig> {
    board::board_config(name).ok_or_else(|| {
        anyhow!(
            "unknown Axvisor board `{name}`; available boards: {}",
            available_board_names().join(", ")
        )
    })
}

pub fn write_defconfig(workspace_root: &Path, board_name: &str) -> anyhow::Result<PathBuf> {
    let board_config = resolve_board_config(board_name)?;
    let build_config_path = default_build_config_path(workspace_root);
    if let Some(parent) = build_config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&build_config_path, toml::to_string_pretty(&board_config)?)?;

    let mut snapshot = AxvisorCommandSnapshot::load(workspace_root)?;
    snapshot.config = Some(PathBuf::from(DEFAULT_BUILD_CONFIG_RELATIVE_PATH));
    snapshot.store(workspace_root)?;

    Ok(build_config_path)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::context::{
        AxvisorQemuSnapshot, AxvisorUbootSnapshot, DEFAULT_AXVISOR_ARCH, DEFAULT_AXVISOR_TARGET,
    };

    #[test]
    fn write_defconfig_generates_build_toml_and_updates_snapshot() {
        let root = tempdir().unwrap();
        let qemu_config = PathBuf::from("configs/qemu.toml");
        let existing_snapshot = AxvisorCommandSnapshot {
            arch: Some(DEFAULT_AXVISOR_ARCH.to_string()),
            target: Some(DEFAULT_AXVISOR_TARGET.to_string()),
            plat_dyn: Some(false),
            config: Some(PathBuf::from("os/axvisor/.build-aarch64.toml")),
            vmconfigs: vec![PathBuf::from("tmp/vm1.toml")],
            qemu: AxvisorQemuSnapshot {
                qemu_config: Some(qemu_config.clone()),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };
        existing_snapshot.store(root.path()).unwrap();

        let path = write_defconfig(root.path(), "roc-rk3568-pc").unwrap();

        assert_eq!(path, root.path().join(DEFAULT_BUILD_CONFIG_RELATIVE_PATH));
        let content = fs::read_to_string(&path).unwrap();
        let parsed = toml::from_str::<AxvisorBoardConfig>(&content).unwrap();
        let expected = board::board_config("roc-rk3568-pc").unwrap();
        assert_eq!(parsed, expected);

        let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(
            snapshot.config,
            Some(PathBuf::from(DEFAULT_BUILD_CONFIG_RELATIVE_PATH))
        );
        assert_eq!(snapshot.arch, existing_snapshot.arch);
        assert_eq!(snapshot.target, existing_snapshot.target);
        assert_eq!(snapshot.plat_dyn, existing_snapshot.plat_dyn);
        assert_eq!(snapshot.vmconfigs, existing_snapshot.vmconfigs);
        assert_eq!(snapshot.qemu.qemu_config, Some(qemu_config));
    }

    #[test]
    fn available_board_names_match_board_default_list_order() {
        assert_eq!(available_board_names(), board::board_names());
    }

    #[test]
    fn resolve_board_config_reports_available_boards_for_unknown_name() {
        let err = resolve_board_config("missing").unwrap_err().to_string();
        assert!(err.contains("unknown Axvisor board `missing`"));
        assert!(err.contains("qemu-aarch64"));
        assert!(err.contains("orangepi-5-plus"));
    }
}

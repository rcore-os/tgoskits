use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    axvisor::board::{self, Board},
    context::{AxvisorCommandSnapshot, snapshot_path_value},
};

pub(crate) fn default_build_config_path(axvisor_dir: &Path) -> PathBuf {
    axvisor_dir.join(".build.toml")
}

pub(crate) fn available_board_names(axvisor_dir: &Path) -> anyhow::Result<Vec<String>> {
    board::board_names(axvisor_dir)
}

fn resolve_board(axvisor_dir: &Path, name: &str) -> anyhow::Result<Board> {
    board::find_board(axvisor_dir, name)?.ok_or_else(|| {
        let available = available_board_names(axvisor_dir).unwrap_or_default();
        anyhow!(
            "unknown Axvisor board `{name}` in {}; available boards: {}",
            board::board_dir(axvisor_dir).display(),
            available.join(", ")
        )
    })
}

pub(crate) fn write_defconfig(
    workspace_root: &Path,
    axvisor_dir: &Path,
    board_name: &str,
) -> anyhow::Result<PathBuf> {
    let board = resolve_board(axvisor_dir, board_name)?;
    let build_config_path = default_build_config_path(axvisor_dir);
    if let Some(parent) = build_config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&board.path, &build_config_path).map_err(|e| {
        anyhow!(
            "failed to copy board config {} to {}: {e}",
            board.path.display(),
            build_config_path.display()
        )
    })?;

    let mut snapshot = AxvisorCommandSnapshot::load(workspace_root)?;
    snapshot.config = Some(snapshot_path_value(workspace_root, &build_config_path));
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

    fn write_board(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = board::board_dir(root).join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn write_defconfig_generates_build_toml_and_updates_snapshot() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        let source = write_board(
            &axvisor_dir,
            "roc-rk3568-pc",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["fs", "rk3568-clk"]
log = "Info"
plat_dyn = true
vm_configs = []
"#,
        );
        let qemu_config = PathBuf::from("configs/qemu.toml");
        let existing_snapshot = AxvisorCommandSnapshot {
            arch: Some(DEFAULT_AXVISOR_ARCH.to_string()),
            target: Some(DEFAULT_AXVISOR_TARGET.to_string()),
            plat_dyn: Some(false),
            smp: None,
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

        let path = write_defconfig(root.path(), &axvisor_dir, "roc-rk3568-pc").unwrap();

        assert_eq!(path, axvisor_dir.join(".build.toml"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            fs::read_to_string(source).unwrap()
        );

        let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(
            snapshot.config,
            Some(PathBuf::from("os/axvisor/.build.toml"))
        );
        assert_eq!(snapshot.arch, existing_snapshot.arch);
        assert_eq!(snapshot.target, existing_snapshot.target);
        assert_eq!(snapshot.plat_dyn, existing_snapshot.plat_dyn);
        assert_eq!(snapshot.vmconfigs, existing_snapshot.vmconfigs);
        assert_eq!(snapshot.qemu.qemu_config, Some(qemu_config));
    }

    #[test]
    fn available_board_names_match_filename_order() {
        let root = tempdir().unwrap();
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
plat_dyn = true
"#,
        );
        write_board(
            root.path(),
            "orangepi-5-plus",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["rk3588-clk"]
log = "Info"
plat_dyn = true
"#,
        );

        assert_eq!(
            available_board_names(root.path()).unwrap(),
            vec!["orangepi-5-plus".to_string(), "qemu-aarch64".to_string()]
        );
    }

    #[test]
    fn resolve_board_config_reports_board_directory_for_unknown_name() {
        let root = tempdir().unwrap();
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
plat_dyn = true
"#,
        );

        let err = resolve_board(root.path(), "missing")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown Axvisor board `missing`"));
        assert!(err.contains(&board::board_dir(root.path()).display().to_string()));
        assert!(err.contains("qemu-aarch64"));
    }
}

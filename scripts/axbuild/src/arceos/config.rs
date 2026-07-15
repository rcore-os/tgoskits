use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;

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
        smp: board.build_config.build_info.max_cpu_num,
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

    let Some(board) = board::default_qemu_board_for_target(workspace_root, package, target)? else {
        return Ok(None);
    };
    write_board_to_default_build_config_at(build_config_path, &board)?;
    update_snapshot_for_board(workspace_root, &board, build_config_path)?;
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

    #[test]
    fn ensure_default_build_config_uses_matching_qemu_board_and_resets_runtime_config() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        let source = r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Warn"
"#;
        write_board(root.path(), "qemu-aarch64", source);
        let existing_snapshot = ArceosCommandSnapshot {
            package: Some("arceos-helloworld".to_string()),
            arch: Some("riscv64".to_string()),
            target: Some("riscv64gc-unknown-none-elf".to_string()),
            smp: None,
            config: None,
            qemu: ArceosQemuSnapshot {
                qemu_config: Some("configs/qemu.toml".into()),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: Some("configs/uboot.toml".into()),
            },
        };
        existing_snapshot.store(root.path()).unwrap();

        let output = root.path().join("tmp/custom-arceos.toml");
        let board = ensure_default_build_config_for_target(
            root.path(),
            "arceos-helloworld",
            "aarch64-unknown-none-softfloat",
            &output,
        )
        .unwrap();

        assert_eq!(board.unwrap().name, "qemu-aarch64");
        assert_eq!(fs::read_to_string(&output).unwrap(), source);
        let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.qemu.qemu_config, None);
        assert_eq!(snapshot.uboot.uboot_config, None);
    }
}

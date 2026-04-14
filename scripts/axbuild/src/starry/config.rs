use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{
    board::{self, Board},
    build,
};
use crate::context::{StarryCommandSnapshot, snapshot_path_value, starry_arch_for_target_checked};

pub(crate) fn available_board_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    board::board_names(workspace_root)
}

fn resolve_board(workspace_root: &Path, name: &str) -> anyhow::Result<Board> {
    board::find_board(workspace_root, name)?.ok_or_else(|| {
        let available = available_board_names(workspace_root).unwrap_or_default();
        anyhow!(
            "unknown Starry board `{name}` in {}; available boards: {}",
            board::board_dir(workspace_root)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "os/StarryOS/configs/board".to_string()),
            available.join(", ")
        )
    })
}

fn write_board_to_default_build_config(
    workspace_root: &Path,
    board: &Board,
) -> anyhow::Result<PathBuf> {
    let build_config_path = build::resolve_build_info_path(workspace_root, &board.target, None)?;
    write_board_to_build_config(&build_config_path, board)?;
    Ok(build_config_path)
}

fn write_board_to_build_config(build_config_path: &Path, board: &Board) -> anyhow::Result<()> {
    if let Some(parent) = build_config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&board.path, build_config_path).map_err(|e| {
        anyhow!(
            "failed to copy Starry board config {} to {}: {e}",
            board.path.display(),
            build_config_path.display()
        )
    })?;
    Ok(())
}

fn update_snapshot_for_board(workspace_root: &Path, board: &Board) -> anyhow::Result<()> {
    let mut snapshot = StarryCommandSnapshot::load(workspace_root)?;
    snapshot.arch = Some(starry_arch_for_target_checked(&board.target)?.to_string());
    snapshot.target = Some(board.target.clone());
    snapshot.qemu.qemu_config = snapshot
        .qemu
        .qemu_config
        .as_ref()
        .map(|path| snapshot_path_value(workspace_root, path));
    snapshot.uboot.uboot_config = snapshot
        .uboot
        .uboot_config
        .as_ref()
        .map(|path| snapshot_path_value(workspace_root, path));
    snapshot.store(workspace_root)?;
    Ok(())
}

pub(crate) fn ensure_default_build_config_for_target(
    workspace_root: &Path,
    target: &str,
    build_config_path: &Path,
) -> anyhow::Result<Option<Board>> {
    if build_config_path.exists() {
        return Ok(None);
    }

    let board = board::default_board_for_target(workspace_root, target)?.ok_or_else(|| {
        anyhow!(
            "missing Starry qemu defconfig for target `{target}`; expected a default qemu board \
             config under {}",
            board::board_dir(workspace_root)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "os/StarryOS/configs/board".to_string())
        )
    })?;
    write_board_to_build_config(build_config_path, &board)?;
    update_snapshot_for_board(workspace_root, &board)?;
    Ok(Some(board))
}

pub(crate) fn write_defconfig(workspace_root: &Path, board_name: &str) -> anyhow::Result<PathBuf> {
    let board = resolve_board(workspace_root, board_name)?;
    let build_config_path = write_board_to_default_build_config(workspace_root, &board)?;
    update_snapshot_for_board(workspace_root, &board)?;
    Ok(build_config_path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::context::{StarryQemuSnapshot, StarryUbootSnapshot};

    fn write_workspace(root: &Path) {
        let starry_workspace_dir = root.join("os/StarryOS");
        let starry_dir = root.join("os/StarryOS/starryos");
        let src_dir = starry_dir.join("src");
        fs::create_dir_all(&starry_workspace_dir).unwrap();
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "").unwrap();
        fs::write(
            starry_dir.join("Cargo.toml"),
            "[package]\nname = \"starryos\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            starry_workspace_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"starryos\"]\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
    }

    fn write_board(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = board::board_dir(root).unwrap().join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn write_defconfig_generates_build_file_and_updates_snapshot() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        let source = write_board(
            root.path(),
            "qemu-riscv64",
            r#"
target = "riscv64gc-unknown-none-elf"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );
        let existing_snapshot = StarryCommandSnapshot {
            arch: Some("aarch64".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            smp: None,
            qemu: StarryQemuSnapshot {
                qemu_config: Some(PathBuf::from("test-suit/starryos/smoke/qemu-riscv64.toml")),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };
        existing_snapshot.store(root.path()).unwrap();

        let path = write_defconfig(root.path(), "qemu-riscv64").unwrap();

        assert_eq!(
            path,
            root.path()
                .join("os/StarryOS/starryos/.build-riscv64gc-unknown-none-elf.toml")
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            fs::read_to_string(source).unwrap()
        );

        let snapshot = StarryCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.arch.as_deref(), Some("riscv64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some("riscv64gc-unknown-none-elf")
        );
        assert_eq!(
            snapshot.qemu.qemu_config,
            Some(PathBuf::from("test-suit/starryos/smoke/qemu-riscv64.toml"))
        );
        assert_eq!(
            snapshot.uboot.uboot_config,
            Some(PathBuf::from("configs/uboot.toml"))
        );
    }

    #[test]
    fn write_defconfig_reports_board_directory_for_unknown_name() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
target = "aarch64-unknown-none-softfloat"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );

        let err = write_defconfig(root.path(), "missing")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown Starry board `missing`"));
        assert!(err.contains(&board::board_dir(root.path()).unwrap().display().to_string()));
        assert!(err.contains("qemu-aarch64"));
    }

    #[test]
    fn ensure_default_build_config_for_target_generates_missing_file_and_updates_snapshot() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        let source = write_board(
            root.path(),
            "qemu-riscv64",
            r#"
target = "riscv64gc-unknown-none-elf"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );
        let existing_snapshot = StarryCommandSnapshot {
            arch: Some("aarch64".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            smp: None,
            qemu: StarryQemuSnapshot::default(),
            uboot: StarryUbootSnapshot::default(),
        };
        existing_snapshot.store(root.path()).unwrap();

        let output = root.path().join("tmp/custom-starry.toml");
        let board = ensure_default_build_config_for_target(
            root.path(),
            "riscv64gc-unknown-none-elf",
            &output,
        )
        .unwrap()
        .unwrap();

        assert_eq!(board.name, "qemu-riscv64");
        assert_eq!(
            fs::read_to_string(&output).unwrap(),
            fs::read_to_string(source).unwrap()
        );

        let snapshot = StarryCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot.arch.as_deref(), Some("riscv64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some("riscv64gc-unknown-none-elf")
        );
    }

    #[test]
    fn ensure_default_build_config_for_target_keeps_existing_file() {
        let root = tempdir().unwrap();
        write_workspace(root.path());
        write_board(
            root.path(),
            "qemu-aarch64",
            r#"
target = "aarch64-unknown-none-softfloat"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["qemu"]
log = "Warn"
plat_dyn = false
"#,
        );

        let output = root.path().join("tmp/custom-starry.toml");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, "plat_dyn = true\n").unwrap();

        let board = ensure_default_build_config_for_target(
            root.path(),
            "aarch64-unknown-none-softfloat",
            &output,
        )
        .unwrap();

        assert!(board.is_none());
        assert_eq!(fs::read_to_string(&output).unwrap(), "plat_dyn = true\n");
    }
}

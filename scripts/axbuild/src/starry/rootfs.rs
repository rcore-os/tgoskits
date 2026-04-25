//! StarryOS-specific rootfs orchestration for run and test flows.
//!
//! Main responsibilities:
//! - Resolve and prepare the default managed rootfs used by StarryOS targets
//! - Patch QEMU configs so StarryOS boots with the expected rootfs image
//! - Build case-specific rootfs assets for C and shell-based Starry test cases
//! - Orchestrate staging roots, overlays, runtime dependency sync, and helper
//!   tooling around rootfs content injection

use std::path::{Path, PathBuf};

use anyhow::bail;
use ostool::run::qemu::QemuConfig;

pub(crate) use crate::rootfs::qemu::{RootfsPatchMode, patch_rootfs};
use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    rootfs::store,
};

/// Ensures the default managed rootfs for a Starry arch/target is available.
pub(crate) async fn ensure_rootfs_in_target_dir(
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<PathBuf> {
    let expected_target = starry_target_for_arch_checked(arch)?;
    if target != expected_target {
        bail!("Starry arch `{arch}` maps to target `{expected_target}`, but got `{target}`");
    }

    store::ensure_rootfs_for_arch(workspace_root, arch).await
}

/// Applies the default Starry rootfs-backed QEMU arguments for a request.
pub(crate) async fn apply_default_qemu_args(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    qemu: &mut QemuConfig,
) -> anyhow::Result<()> {
    let disk_img =
        ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    patch_rootfs(qemu, &disk_img, RootfsPatchMode::EnsureDiskBootNet);
    Ok(())
}

/// Applies or replaces the `-smp` argument in a QEMU config.
pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    if let Some(index) = qemu.args.iter().position(|arg| arg == "-smp")
        && let Some(value) = qemu.args.get_mut(index + 1)
    {
        *value = cpu_num.to_string();
        return;
    }

    qemu.args.push("-smp".to_string());
    qemu.args.push(cpu_num.to_string());
}

/// Reads the effective CPU count from a QEMU `-smp` argument, if present.
pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let index = qemu.args.iter().position(|arg| arg == "-smp")?;
    let value = qemu.args.get(index + 1)?;
    parse_smp_qemu_value(value)
}

/// Parses the CPU count encoded in a QEMU `-smp` value.
fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}
#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn apply_default_qemu_args_includes_rootfs_and_network_defaults() {
        let root = tempdir().unwrap();
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(rootfs_dir.join("rootfs-x86_64-alpine.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig::default();

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/rootfs/rootfs-x86_64-alpine.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
        assert_eq!(
            fs::read(root.path().join("target/rootfs/rootfs-x86_64-alpine.img")).unwrap(),
            b"rootfs"
        );
    }

    #[tokio::test]
    async fn apply_default_qemu_args_preserves_existing_base_args() {
        let root = tempdir().unwrap();
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(rootfs_dir.join("rootfs-riscv64-alpine.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "riscv64".to_string(),
            target: "riscv64gc-unknown-none-elf".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig {
            args: vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
            ],
            ..Default::default()
        };

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/rootfs/rootfs-riscv64-alpine.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
    }

    #[test]
    fn apply_smp_qemu_arg_appends_cpu_count() {
        let mut qemu = QemuConfig {
            args: vec!["-machine".to_string(), "virt".to_string()],
            ..Default::default()
        };

        apply_smp_qemu_arg(&mut qemu, Some(4));

        assert_eq!(
            qemu.args,
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ]
        );
    }

    #[test]
    fn apply_smp_qemu_arg_replaces_existing_cpu_count() {
        let mut qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "1".to_string(),
            ],
            ..Default::default()
        };

        apply_smp_qemu_arg(&mut qemu, Some(4));

        assert_eq!(
            qemu.args,
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ]
        );
    }

    #[test]
    fn smp_from_qemu_arg_reads_plain_cpu_count() {
        let qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), Some(4));
    }

    #[test]
    fn smp_from_qemu_arg_reads_cpus_key_value_syntax() {
        let qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "q35".to_string(),
                "-smp".to_string(),
                "cpus=3,sockets=1,cores=3,threads=1".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), Some(3));
    }

    #[test]
    fn smp_from_qemu_arg_returns_none_when_missing() {
        let qemu = QemuConfig {
            args: vec!["-machine".to_string(), "q35".to_string()],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), None);
    }
}

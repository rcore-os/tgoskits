//! StarryOS-specific rootfs orchestration for run and test flows.
//!
//! Main responsibilities:
//! - Resolve and prepare the default managed rootfs used by StarryOS targets
//! - Patch QEMU configs so StarryOS boots with the expected rootfs image

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;

use super::apk;
pub(crate) use crate::rootfs::qemu::{RootfsPatchMode, patch_rootfs};
use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    rootfs::{inject, store},
};

const APK_REPOSITORIES_PATH: &str = "/etc/apk/repositories";
const QEMU_SLIRP_RESOLV_CONF: &str = "nameserver 10.0.2.3\n";
const EXT_SUPER_MAGIC_OFFSET: u64 = 1080;
const EXT_SUPER_MAGIC: [u8; 2] = [0x53, 0xef];

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

    let rootfs = store::ensure_rootfs_for_arch(workspace_root, arch).await?;
    ensure_apk_region_in_rootfs(&rootfs)?;
    Ok(rootfs)
}

/// Ensures a selected rootfs image exists without modifying its contents.
pub(crate) async fn ensure_qemu_rootfs_ready(
    request: &ResolvedStarryRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    store::ensure_optional_managed_rootfs(workspace_root, &request.arch, Some(&rootfs_path)).await
}

fn ensure_apk_region_in_rootfs(rootfs_img: &Path) -> anyhow::Result<()> {
    if !looks_like_ext_image(rootfs_img)? {
        return Ok(());
    }

    let Some(original) = inject::read_text_file(rootfs_img, APK_REPOSITORIES_PATH)? else {
        sync_qemu_slirp_resolver_in_rootfs(rootfs_img)?;
        return Ok(());
    };
    let region = apk::apk_region_from_env()?;
    let rewritten = apk::rewrite_apk_repositories_content(&original, region);
    replace_rootfs_text_file_if_changed(rootfs_img, APK_REPOSITORIES_PATH, &rewritten)?;
    sync_qemu_slirp_resolver_in_rootfs(rootfs_img)
}

fn sync_qemu_slirp_resolver_in_rootfs(rootfs_img: &Path) -> anyhow::Result<()> {
    replace_rootfs_text_file_if_changed(rootfs_img, "/etc/resolv.conf", QEMU_SLIRP_RESOLV_CONF)
}

fn replace_rootfs_text_file_if_changed(
    rootfs_img: &Path,
    guest_path: &str,
    content: &str,
) -> anyhow::Result<()> {
    if inject::read_text_file(rootfs_img, guest_path)?.as_deref() == Some(content) {
        return Ok(());
    }

    let temp_purpose = guest_path.trim_start_matches('/').replace('/', "-");
    let temp_path = unique_temp_file_path(rootfs_img, &temp_purpose)?;
    fs::write(&temp_path, content)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("failed to chmod {}", temp_path.display()))?;
    }
    let replace_result = inject::replace_file(rootfs_img, guest_path, &temp_path);
    let cleanup_result = fs::remove_file(&temp_path)
        .with_context(|| format!("failed to remove {}", temp_path.display()));

    replace_result?;
    cleanup_result?;
    Ok(())
}

fn looks_like_ext_image(path: &Path) -> anyhow::Result<bool> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    if file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len()
        < EXT_SUPER_MAGIC_OFFSET + EXT_SUPER_MAGIC.len() as u64
    {
        return Ok(false);
    }

    let mut magic = [0_u8; 2];
    file.seek(SeekFrom::Start(EXT_SUPER_MAGIC_OFFSET))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    file.read_exact(&mut magic)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(magic == EXT_SUPER_MAGIC)
}

fn unique_temp_file_path(rootfs_img: &Path, purpose: &str) -> anyhow::Result<PathBuf> {
    let dir = rootfs_img
        .parent()
        .ok_or_else(|| anyhow::anyhow!("rootfs image has no parent: {}", rootfs_img.display()))?;
    let image_name = rootfs_img
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid rootfs image path: {}", rootfs_img.display()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_nanos();
    Ok(dir.join(format!(
        ".{image_name}.{purpose}.{}.{}.tmp",
        std::process::id(),
        nanos
    )))
}

/// Patches a QEMU config with the rootfs selected for a Starry request.
pub(crate) fn patch_qemu_rootfs(
    qemu: &mut QemuConfig,
    request: &ResolvedStarryRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let expected_target = starry_target_for_arch_checked(&request.arch)?;
    if request.target != expected_target {
        bail!(
            "Starry arch `{}` maps to target `{expected_target}`, but got `{}`",
            request.arch,
            request.target
        );
    }
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    patch_qemu_rootfs_path(qemu, &rootfs_path);
    Ok(())
}

/// Resolves the rootfs path selected for a Starry QEMU request.
pub(crate) fn qemu_rootfs_path(
    request: &ResolvedStarryRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit_rootfs {
        return Ok(explicit.to_path_buf());
    }

    store::default_rootfs_path(workspace_root, &request.arch)
}

/// Patches a QEMU config with a concrete Starry rootfs path.
pub(crate) fn patch_qemu_rootfs_path(qemu: &mut QemuConfig, rootfs_path: &Path) {
    patch_rootfs(qemu, rootfs_path, RootfsPatchMode::EnsureDiskBootNet);
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn patch_qemu_rootfs_includes_rootfs_and_network_defaults() {
        let root = tempdir().unwrap();
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(
            rootfs_dir.join("rootfs-x86_64-alpine.img"),
            vec![0; 1024 * 1024],
        )
        .unwrap();

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

        patch_qemu_rootfs(&mut qemu, &request, root.path(), None).unwrap();

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
        assert!(
            root.path()
                .join("target/rootfs/rootfs-x86_64-alpine.img")
                .exists()
        );
    }

    #[tokio::test]
    async fn patch_qemu_rootfs_preserves_existing_base_args() {
        let root = tempdir().unwrap();
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(
            rootfs_dir.join("rootfs-riscv64-alpine.img"),
            vec![0; 1024 * 1024],
        )
        .unwrap();

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

        patch_qemu_rootfs(&mut qemu, &request, root.path(), None).unwrap();

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
}

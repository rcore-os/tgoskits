//! ArceOS-specific rootfs helpers for QEMU runs.
//!
//! Main responsibilities:
//! - Resolve explicit ArceOS rootfs CLI values into concrete image paths
//! - Ensure managed rootfs images are available before launch
//! - Patch QEMU configs so an explicit rootfs image is attached correctly

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::Context;
use ostool::run::qemu::QemuConfig;

use super::{ArceOS, build};
use crate::{context::ResolvedBuildRequest, test::qemu as qemu_test};

const DEFAULT_FAT32_ROOTFS_SIZE: &str = "64M";

/// Prepares ArceOS's default FAT32 rootfs referenced by a QEMU config.
///
/// ArceOS keeps this compatibility path separate from image-managed rootfs
/// handling until the ArceOS, StarryOS, and Axvisor filesystem contracts are
/// unified. An explicit `--rootfs` still overrides this default path.
pub(crate) fn prepare_default_qemu_fat32_rootfs(
    workspace_root: &Path,
    qemu: &QemuConfig,
) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for image in qemu_fat32_rootfs_images(qemu) {
        if seen.insert(image.clone()) {
            ensure_fat32_rootfs(
                &image,
                should_recreate_runtime_image(workspace_root, &image),
            )?;
        }
    }
    Ok(())
}

fn qemu_fat32_rootfs_images(qemu: &QemuConfig) -> Vec<PathBuf> {
    crate::rootfs::qemu::drive_file_paths(qemu)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == "disk.img"
                        || name.starts_with("arceos-") && name.ends_with("-fat32.img")
                })
        })
        .collect()
}

fn should_recreate_runtime_image(workspace_root: &Path, image: &Path) -> bool {
    image.starts_with(crate::context::axbuild_tmp_dir(workspace_root).join("runtime-assets"))
}

fn ensure_fat32_rootfs(image: &Path, recreate: bool) -> anyhow::Result<()> {
    if image.exists() && !recreate {
        return Ok(());
    }
    let message = format!("generating FAT32 rootfs {}", image.display());
    println!("{message} ...");
    if let Some(parent) = image.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if image.exists() {
        std::fs::remove_file(image)?;
    }
    run_fat32_creation_command(
        StdCommand::new("truncate")
            .args(["-s", DEFAULT_FAT32_ROOTFS_SIZE])
            .arg(image),
    )?;
    run_fat32_creation_command(
        StdCommand::new("mkfs.fat")
            .args(["-F", "32"])
            .arg(image)
            .stdout(std::process::Stdio::null()),
    )?;
    println!("{message} ... done");
    Ok(())
}

fn run_fat32_creation_command(command: &mut StdCommand) -> anyhow::Result<()> {
    let name = command.get_program().to_string_lossy().to_string();
    command
        .status()
        .with_context(|| format!("failed to run `{name}`"))?
        .success()
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("`{name}` exited with non-zero status"))
}

/// Resolves an explicit ArceOS rootfs CLI value into a concrete path.
pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> anyhow::Result<PathBuf> {
    crate::image::storage::resolve_rootfs_path(workspace_root, arch, rootfs)
}

/// Ensures a managed ArceOS rootfs image is available before launch.
pub(crate) async fn ensure_rootfs_ready(
    workspace_root: &Path,
    arch: &str,
    rootfs: &Path,
) -> anyhow::Result<()> {
    crate::image::storage::ensure_managed_rootfs(workspace_root, arch, rootfs).await
}

/// Patches a QEMU config so it boots with the selected ArceOS rootfs image.
pub(crate) fn patch_qemu_rootfs(qemu: &mut QemuConfig, rootfs: &Path) {
    crate::rootfs::qemu::patch_rootfs(
        qemu,
        rootfs,
        crate::rootfs::qemu::RootfsPatchMode::EnsureDiskBootNet,
    );
}

pub(super) async fn qemu_with_explicit_rootfs(
    arceos: &mut ArceOS,
    request: ResolvedBuildRequest,
    rootfs: PathBuf,
) -> anyhow::Result<()> {
    let rootfs = resolve_explicit_rootfs(arceos.app.workspace_root(), &request.arch, rootfs)?;
    ensure_rootfs_ready(arceos.app.workspace_root(), &request.arch, &rootfs).await?;
    arceos.app.set_debug_mode(request.debug)?;
    let cargo = build::load_cargo_config(&request)?;
    let mut qemu = arceos
        .load_qemu_config(&request, &cargo)
        .await?
        .unwrap_or_default();
    qemu_test::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
    patch_qemu_rootfs(&mut qemu, &rootfs);
    qemu_test::apply_smp_qemu_arg(&mut qemu, request.smp);
    arceos
        .app
        .qemu(cargo, request.build_info_path, Some(qemu))
        .await
}

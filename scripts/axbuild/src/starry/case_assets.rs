//! StarryOS test case asset orchestration.
//!
//! Main responsibilities:
//! - Decide whether a test case needs extra build or injection work
//! - Prepare case-scoped work directories, overlays, and auxiliary QEMU assets
//! - Dispatch C and shell case flows before rootfs content injection

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};

use super::{case_build, test_suit::StarryQemuCase};

const CASE_WORK_ROOT_NAME: &str = "starry-cases";
const CASE_STAGING_DIR_NAME: &str = "staging-root";
const CASE_BUILD_DIR_NAME: &str = "build";
const CASE_OVERLAY_DIR_NAME: &str = "overlay";
const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
const CASE_SH_DIR_NAME: &str = "sh";
const USB_STICK_IMAGE_NAME: &str = "usb-stick.raw";
const USB_STICK_IMAGE_SIZE: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryCaseAssets {
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseAssetLayout {
    pub(crate) work_dir: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) overlay_dir: PathBuf,
    pub(crate) command_wrapper_dir: PathBuf,
    pub(crate) cross_bin_dir: PathBuf,
    pub(crate) cmake_toolchain_file: PathBuf,
    pub(crate) apk_cache_dir: PathBuf,
    pub(crate) usb_stick_path: PathBuf,
}

/// Resolves the workspace target directory used for a Starry build target.
pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let _ = crate::context::starry_arch_for_target_checked(target)?;
    Ok(workspace_root.join("target").join(target))
}

/// Prepares any case-specific rootfs assets required by a Starry QEMU test.
pub(crate) async fn prepare_case_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
    rootfs_path: PathBuf,
) -> anyhow::Result<StarryCaseAssets> {
    let needs_assets = case_uses_c_pipeline(case)
        || case_uses_sh_pipeline(case)
        || case_uses_usb_qemu_assets(arch, case);

    if !needs_assets {
        return Ok(StarryCaseAssets {
            rootfs_path,
            extra_qemu_args: Vec::new(),
        });
    }

    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let rootfs_path_for_task = rootfs_path.clone();
    let case = case.clone();
    let extra_qemu_args = tokio::task::spawn_blocking(move || {
        prepare_case_assets_sync(
            &workspace_root,
            &arch,
            &target,
            &case,
            &rootfs_path_for_task,
        )
    })
    .await
    .context("starry case asset task failed")??;

    Ok(StarryCaseAssets {
        rootfs_path,
        extra_qemu_args,
    })
}

/// Returns whether a Starry test case uses the C pipeline.
pub(crate) fn case_uses_c_pipeline(case: &StarryQemuCase) -> bool {
    case_build::case_c_source_dir(case).is_dir()
}

/// Returns the shell-script source directory for a Starry test case.
pub(crate) fn case_sh_source_dir(case: &StarryQemuCase) -> PathBuf {
    case.case_dir.join(CASE_SH_DIR_NAME)
}

/// Returns whether a Starry test case uses the shell pipeline.
pub(crate) fn case_uses_sh_pipeline(case: &StarryQemuCase) -> bool {
    case_sh_source_dir(case).is_dir()
}

/// Returns whether a Starry test case needs extra USB-backed QEMU assets.
pub(crate) fn case_uses_usb_qemu_assets(arch: &str, case: &StarryQemuCase) -> bool {
    let _ = arch;
    let _ = case;
    false
}

/// Builds the working directory layout used for a Starry case asset run.
pub(crate) fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);

    Ok(CaseAssetLayout {
        staging_root: work_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: work_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: work_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: work_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: work_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: work_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: work_dir.join(CASE_APK_CACHE_DIR_NAME),
        usb_stick_path: work_dir.join(USB_STICK_IMAGE_NAME),
        work_dir,
    })
}

/// Performs the synchronous part of Starry case asset preparation.
pub(crate) fn prepare_case_assets_sync(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
    case_rootfs: &Path,
) -> anyhow::Result<Vec<String>> {
    let layout = case_asset_layout(workspace_root, target, &case.name)?;
    fs::create_dir_all(&layout.work_dir)
        .with_context(|| format!("failed to create {}", layout.work_dir.display()))?;

    if case_uses_c_pipeline(case) {
        case_build::prepare_c_case_assets_sync(arch, case, case_rootfs, &layout)?;
    } else if case_uses_sh_pipeline(case) {
        prepare_sh_case_assets_sync(case, case_rootfs, &layout)?;
    }

    let mut extra_qemu_args = Vec::new();
    if case_uses_usb_qemu_assets(arch, case) {
        create_usb_backing_image(&layout.usb_stick_path)?;
        extra_qemu_args.extend(usb_qemu_args(&layout.usb_stick_path));
    }

    Ok(extra_qemu_args)
}

/// Prepares overlay assets for a Starry shell-based test case.
pub(crate) fn prepare_sh_case_assets_sync(
    case: &StarryQemuCase,
    case_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    let sh_dir = case_sh_source_dir(case);
    ensure!(
        sh_dir.is_dir(),
        "sh directory not found at `{}`",
        sh_dir.display()
    );

    reset_dir(&layout.overlay_dir)?;

    let dest_dir = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;

    let mut entries = fs::read_dir(&sh_dir)
        .with_context(|| format!("failed to read {}", sh_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", sh_dir.display()))?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_dir.join(entry.file_name());
        fs::copy(&path, &dest)
            .with_context(|| format!("failed to copy {} to {}", path.display(), dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dest)
                .with_context(|| format!("failed to stat {}", dest.display()))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms)
                .with_context(|| format!("failed to chmod {}", dest.display()))?;
        }
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Returns the extra QEMU arguments used for the synthetic USB backing image.
pub(crate) fn usb_qemu_args(usb_stick_path: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "qemu-xhci,id=xhci".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,format=raw,file={},id=usbstick0",
            usb_stick_path.display()
        ),
        "-device".to_string(),
        "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
    ]
}

/// Creates the empty backing image used for USB-related QEMU test assets.
pub(crate) fn create_usb_backing_image(path: &Path) -> anyhow::Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.set_len(USB_STICK_IMAGE_SIZE)
        .with_context(|| format!("failed to size {}", path.display()))
}

/// Resets a directory to an empty existing state.
pub(crate) fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn fake_case(root: &Path, name: &str) -> StarryQemuCase {
        let case_dir = root.join("test-suit/starryos/normal").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        StarryQemuCase {
            name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
            build_config_path: None,
        }
    }

    #[test]
    fn resolve_target_dir_uses_workspace_target_directory() {
        let root = tempdir().unwrap();
        let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

        assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
    }

    #[tokio::test]
    async fn prepare_case_assets_keeps_default_cases_plain() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(rootfs_dir.join("rootfs-x86_64-alpine.img"), b"rootfs").unwrap();
        let case = fake_case(root.path(), "smoke");

        let assets = prepare_case_assets(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            &case,
            rootfs_dir.join("rootfs-x86_64-alpine.img"),
        )
        .await
        .unwrap();

        assert_eq!(
            assets.rootfs_path,
            rootfs_dir.join("rootfs-x86_64-alpine.img")
        );
        assert!(assets.extra_qemu_args.is_empty());
        assert_eq!(fs::read(&assets.rootfs_path).unwrap(), b"rootfs");
        assert!(!target_dir.join("rootfs-x86_64-smoke.img").exists());
    }

    #[test]
    fn case_asset_layout_and_usb_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();

        assert_eq!(
            layout.work_dir,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-cases/usb")
        );
        assert_eq!(
            usb_qemu_args(&layout.usb_stick_path),
            vec![
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-drive".to_string(),
                format!(
                    "if=none,format=raw,file={},id=usbstick0",
                    layout.usb_stick_path.display()
                ),
                "-device".to_string(),
                "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
            ]
        );
    }
}

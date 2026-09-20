use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};
use tempfile::TempDir;

use super::{
    super::rootfs,
    types::{AppOwnedRootfsPreparation, RootfsPreparation, StarryAppCase},
};
use crate::{rootfs::inject, support::process::ProcessExt, test::case::copy_file_fast};

#[derive(Debug)]
pub(super) struct PreparedAppRootfs {
    pub(super) path: PathBuf,
    pub(super) cleanup_dir: Option<PathBuf>,
}

impl PreparedAppRootfs {
    fn borrowed(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_dir: None,
        }
    }
}

#[derive(Debug)]
struct DefaultAppRootfsRun {
    directory: TempDir,
    rootfs_path: PathBuf,
    staging_root: PathBuf,
    overlay_dir: PathBuf,
}

impl DefaultAppRootfsRun {
    fn finish(self) -> PreparedAppRootfs {
        PreparedAppRootfs {
            path: self.rootfs_path,
            cleanup_dir: Some(self.directory.keep()),
        }
    }
}

pub(super) async fn prepare_qemu_app_rootfs(
    workspace_root: &Path,
    target_dir: &Path,
    app: &StarryAppCase,
    arch: &str,
    target: &str,
    configured_rootfs: Option<&Path>,
    preparation: &RootfsPreparation,
) -> anyhow::Result<PreparedAppRootfs> {
    match preparation {
        RootfsPreparation::Default => {
            prepare_default_qemu_app_rootfs(
                workspace_root,
                target_dir,
                app,
                arch,
                target,
                configured_rootfs,
            )
            .await
        }
        RootfsPreparation::AppOwned(config) => prepare_app_owned_qemu_rootfs(
            workspace_root,
            app,
            arch,
            target,
            configured_rootfs,
            config,
        ),
    }
}

async fn prepare_default_qemu_app_rootfs(
    workspace_root: &Path,
    target_dir: &Path,
    app: &StarryAppCase,
    arch: &str,
    target: &str,
    configured_rootfs: Option<&Path>,
) -> anyhow::Result<PreparedAppRootfs> {
    let rootfs_path = match configured_rootfs {
        Some(path) => path.to_path_buf(),
        None => crate::image::storage::default_rootfs_path(workspace_root, target_dir, arch)?,
    };
    if app.prebuild_path.is_none() {
        if let Some(configured) = configured_rootfs {
            crate::image::storage::ensure_optional_managed_rootfs(
                workspace_root,
                target_dir,
                arch,
                Some(configured),
            )
            .await?;
            rootfs::ensure_apk_region_in_rootfs(configured)?;
            return Ok(PreparedAppRootfs::borrowed(configured.to_path_buf()));
        }
        return rootfs::ensure_rootfs_in_tmp_dir(workspace_root, target_dir, arch, target)
            .await
            .map(PreparedAppRootfs::borrowed);
    }

    let default_rootfs =
        rootfs::ensure_rootfs_in_tmp_dir(workspace_root, target_dir, arch, target).await?;
    let run = create_default_app_rootfs_run(workspace_root, app, &default_rootfs, &rootfs_path)?;
    inject::set_directory_owner_and_mode(&run.rootfs_path, "/root", 0, 0, 0o700)?;

    let prepare_result = (|| -> anyhow::Result<()> {
        reset_dir(&run.staging_root)?;
        reset_dir(&run.overlay_dir)?;

        if let Some(prebuild_path) = app.prebuild_path.as_deref() {
            let mut command = Command::new("bash");
            command
                .arg(prebuild_path)
                .current_dir(&app.case_dir)
                .env("STARRY_APP_NAME", &app.name)
                .env("STARRY_APP_DIR", &app.case_dir)
                .env("STARRY_WORKSPACE", workspace_root)
                .env("STARRY_ARCH", arch)
                .env("STARRY_ROOTFS", &run.rootfs_path)
                .env("STARRY_STAGING_ROOT", &run.staging_root)
                .env("STARRY_OVERLAY_DIR", &run.overlay_dir);
            command
                .exec()
                .with_context(|| format!("failed to run {}", prebuild_path.display()))?;
        }

        inject::inject_overlay(&run.rootfs_path, &run.overlay_dir)
    })();
    prepare_result?;
    Ok(run.finish())
}

fn create_default_app_rootfs_run(
    workspace_root: &Path,
    app: &StarryAppCase,
    default_rootfs: &Path,
    configured_rootfs: &Path,
) -> anyhow::Result<DefaultAppRootfsRun> {
    let runs_dir = workspace_root
        .join("tmp/axbuild/starry-app")
        .join(&app.name)
        .join("runs");
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("failed to create {}", runs_dir.display()))?;
    let directory = tempfile::Builder::new()
        .prefix("rootfs-")
        .tempdir_in(&runs_dir)
        .with_context(|| format!("failed to create a run directory in {}", runs_dir.display()))?;
    let image_name = configured_rootfs.file_name().with_context(|| {
        format!(
            "rootfs path has no file name: {}",
            configured_rootfs.display()
        )
    })?;
    let rootfs_path = directory.path().join(image_name);
    copy_file_fast(default_rootfs, &rootfs_path)?;

    Ok(DefaultAppRootfsRun {
        staging_root: directory.path().join("staging-root"),
        overlay_dir: directory.path().join("overlay"),
        directory,
        rootfs_path,
    })
}

fn prepare_app_owned_qemu_rootfs(
    workspace_root: &Path,
    app: &StarryAppCase,
    arch: &str,
    target: &str,
    configured_rootfs: Option<&Path>,
    config: &AppOwnedRootfsPreparation,
) -> anyhow::Result<PreparedAppRootfs> {
    ensure!(
        config.target_arch == arch,
        "app-owned rootfs for `{}` targets `{}` but QEMU requested `{arch}`",
        app.name,
        config.target_arch
    );
    let rootfs_path = configured_rootfs.with_context(|| {
        format!(
            "app-owned rootfs for `{}` requires a managed rootfs drive in its QEMU config",
            app.name
        )
    })?;
    if let Some(parent) = rootfs_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut command = Command::new("bash");
    command
        .arg(&config.builder_path)
        .current_dir(&app.case_dir)
        .env("STARRY_APP_NAME", &app.name)
        .env("STARRY_APP_DIR", &app.case_dir)
        .env("STARRY_WORKSPACE", workspace_root)
        .env("STARRY_ARCH", arch)
        .env("STARRY_TARGET", target)
        .env("STARRY_ROOTFS", rootfs_path);
    command
        .exec()
        .with_context(|| format!("failed to run {}", config.builder_path.display()))?;

    let metadata = fs::metadata(rootfs_path).with_context(|| {
        format!(
            "app-owned rootfs builder {} did not publish {}",
            config.builder_path.display(),
            rootfs_path.display()
        )
    })?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "app-owned rootfs builder {} published invalid output {}",
        config.builder_path.display(),
        rootfs_path.display()
    );
    Ok(PreparedAppRootfs::borrowed(rootfs_path.to_path_buf()))
}

fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prebuild_runs_use_fresh_rootfs_copies() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("rootfs-aarch64-alpine.img");
        fs::write(&source, b"pristine-rootfs").unwrap();
        let configured = workspace.path().join("rootfs-aarch64-dropbear.img");
        let app = StarryAppCase {
            name: "dropbear".to_string(),
            kind: super::super::types::StarryAppKind::Qemu,
            case_dir: workspace.path().join("apps/starry/dropbear"),
            prebuild_path: Some(workspace.path().join("prebuild.sh")),
            requires: Vec::new(),
        };

        let first =
            create_default_app_rootfs_run(workspace.path(), &app, &source, &configured).unwrap();
        let first_dir = first.directory.path().to_path_buf();
        fs::write(&first.rootfs_path, b"mutated-rootfs").unwrap();

        let second =
            create_default_app_rootfs_run(workspace.path(), &app, &source, &configured).unwrap();
        let second_dir = second.directory.path().to_path_buf();

        assert_ne!(first.rootfs_path, second.rootfs_path);
        assert_eq!(fs::read(&second.rootfs_path).unwrap(), b"pristine-rootfs");
        assert_eq!(fs::read(&source).unwrap(), b"pristine-rootfs");

        drop(first);
        drop(second);
        assert!(!first_dir.exists());
        assert!(!second_dir.exists());
    }
}

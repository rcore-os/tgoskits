use std::path::{Path, PathBuf};

use anyhow::Context;
use ostool::run::qemu::QemuConfig;

use crate::image::config::ImageConfig;

/// Owns the extracted images for one test build group. Compressed downloads
/// remain shared, while every exit path releases the group's mutable images
/// and its derived case assets, including locally cached injected images.
pub(super) struct TestRootfsScope {
    directory: tempfile::TempDir,
    config: ImageConfig,
}

impl TestRootfsScope {
    pub(super) fn new(config: &ImageConfig, target_dir: &Path) -> anyhow::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("starry-test-rootfs-")
            .tempdir_in(target_dir)
            .context("failed to create test-group rootfs directory")?;
        let mut config = config.clone();
        config.extract_dir = directory.path().to_path_buf();
        Ok(Self { directory, config })
    }

    pub(super) fn config(&self) -> &ImageConfig {
        &self.config
    }

    pub(super) fn image_path(&self, image: &Path) -> anyhow::Result<PathBuf> {
        let filename = image
            .file_name()
            .context("managed rootfs has no filename")?;
        Ok(self.config.extract_dir.join(filename))
    }

    pub(super) fn rewrite_qemu_images(
        &self,
        workspace_root: &Path,
        target_dir: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut images = Vec::new();
        crate::rootfs::qemu::rewrite_drive_file_paths(qemu, |path| {
            let Some(managed) = crate::image::storage::resolve_managed_rootfs_path(
                workspace_root,
                target_dir,
                path,
            )?
            else {
                return Ok(None);
            };
            let path = self.image_path(&managed)?;
            images.push(path.clone());
            Ok(Some(path))
        })?;
        Ok(images)
    }

    pub(super) fn close(self) -> anyhow::Result<()> {
        let path = self.directory.path().to_path_buf();
        self.directory
            .close()
            .with_context(|| format!("failed to remove test-group rootfs {}", path.display()))?;
        println!("released test-group rootfs: {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn groups_release_owned_images_and_preserve_shared_inputs() {
        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("target");
        fs::create_dir(&target).unwrap();
        let mut config = ImageConfig::new_default(&target);
        config.download_dir = workspace.path().join("downloads");
        ImageConfig::write_config(workspace.path(), &config).unwrap();
        fs::create_dir_all(&config.extract_dir).unwrap();
        fs::create_dir_all(&config.download_dir).unwrap();
        let archive = config.download_dir.join("fixture.tar.xz");
        fs::write(&archive, b"shared archive").unwrap();
        let shared = config.extract_dir.join("rootfs-x86_64-fixture.img");
        fs::write(&shared, b"shared image").unwrap();
        let auxiliary = config.extract_dir.join("rootfs-x86_64-auxiliary.img");
        fs::write(&auxiliary, b"shared auxiliary image").unwrap();
        let external = workspace.path().join("external.img");
        fs::write(&external, b"user image").unwrap();

        for fail in [false, true] {
            let scope = TestRootfsScope::new(&config, &target).unwrap();
            let group_dir = scope.config().extract_dir.clone();
            let mut qemu = QemuConfig {
                args: vec![
                    "-drive".into(),
                    format!("id=root,file={},format=raw", shared.display()),
                    format!("-drive=id=aux,file={},format=raw", auxiliary.display()),
                    "-drive".into(),
                    format!("id=user,file={},format=raw", external.display()),
                ],
                ..Default::default()
            };
            let images = scope
                .rewrite_qemu_images(workspace.path(), &target, &mut qemu)
                .unwrap();
            assert_eq!(images.len(), 2);
            for image in &images {
                assert!(image.starts_with(&group_dir));
                fs::write(image, b"group image").unwrap();
            }
            let cache = group_dir.join("x86_64/qemu-cases/fixture/cache/rootfs");
            fs::create_dir_all(&cache).unwrap();
            fs::write(cache.join("injected.img"), b"derived image").unwrap();
            let drives = crate::rootfs::qemu::drive_file_paths(&qemu);
            assert!(drives.contains(&images[0]));
            assert!(drives.contains(&external));

            let result: anyhow::Result<()> = (|| {
                if fail {
                    anyhow::bail!("fixture group failed");
                }
                scope.close()
            })();
            assert_eq!(result.is_err(), fail);
            assert!(!group_dir.exists());
            assert_eq!(fs::read(&shared).unwrap(), b"shared image");
            assert_eq!(fs::read(&auxiliary).unwrap(), b"shared auxiliary image");
            assert_eq!(fs::read(&external).unwrap(), b"user image");
            assert_eq!(fs::read(&archive).unwrap(), b"shared archive");
        }
    }
}

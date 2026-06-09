//! Managed rootfs image storage and retrieval helpers for the TGOS image layer.
//!
//! Main responsibilities:
//! - Define default naming rules for workspace-managed rootfs images
//! - Resolve user-facing `--rootfs` values into concrete image paths
//! - Manage installed image files under `tmp/axbuild/rootfs/`
//! - Install registry-managed rootfs images pulled by the TGOS image storage

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use tokio::fs as tokio_fs;

use crate::{
    image::{config::ImageConfig, registry::ImageEntry, spec::ImageSpecRef, storage::Storage},
    support::download::verify_file_sha256,
};

const ROOTFS_SOURCE_SHA256_SUFFIX: &str = ".source.sha256";

/// Returns the default managed rootfs image filename for a given architecture.
pub(crate) fn default_rootfs_image(arch: &str) -> Option<&'static str> {
    crate::context::default_rootfs_image_for_arch(arch)
}

/// Returns the workspace directory that stores managed rootfs images.
pub(crate) fn rootfs_dir(workspace_root: &Path) -> PathBuf {
    crate::context::axbuild_tmp_dir(workspace_root).join("rootfs")
}

/// Resolves a user-facing rootfs argument into a concrete image path.
///
/// Bare values such as `alpine` or `debian` are expanded into the managed
/// `rootfs-<arch>-<distro>.img` naming scheme. Paths that already contain a
/// directory component are treated as explicit filesystem paths.
pub(crate) fn resolve_rootfs_path(workspace_root: &Path, arch: &str, rootfs: PathBuf) -> PathBuf {
    let is_bare = rootfs
        .parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true);

    if !is_bare {
        return rootfs;
    }

    let keyword = rootfs.to_string_lossy();
    let distro = match keyword.as_ref() {
        "alpine" => Some("alpine"),
        "busybox" => Some("busybox"),
        "debian" => Some("debian"),
        _ => None,
    };

    let image_name = if let Some(distro) = distro {
        format!("rootfs-{arch}-{distro}.img")
    } else {
        keyword.into_owned()
    };

    rootfs_dir(workspace_root).join(image_name)
}

/// Resolves an explicit `--rootfs` CLI value into a concrete image path.
pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> PathBuf {
    resolve_rootfs_path(workspace_root, arch, rootfs)
}

/// Returns the default managed rootfs path for an architecture.
pub(crate) fn default_rootfs_path(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let image_name = default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
    Ok(rootfs_dir(workspace_root).join(image_name))
}

/// Ensures a managed rootfs path exists locally before it is used.
///
/// Paths outside the managed rootfs directory are treated as user-managed and
/// therefore skipped.
pub(crate) async fn ensure_managed_rootfs(
    workspace_root: &Path,
    arch: &str,
    path: &Path,
) -> anyhow::Result<()> {
    if !path.starts_with(rootfs_dir(workspace_root)) || default_rootfs_image(arch).is_none() {
        return Ok(());
    }

    let image_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid managed rootfs path `{}`", path.display()))?;
    ensure_rootfs_image(workspace_root, image_name).await?;
    Ok(())
}

/// Ensures an optional managed rootfs path exists locally before it is used.
pub(crate) async fn ensure_optional_managed_rootfs(
    workspace_root: &Path,
    arch: &str,
    path: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(path) = path {
        ensure_managed_rootfs(workspace_root, arch, path).await?;
    }
    Ok(())
}

/// Ensures the default managed rootfs image for an architecture is available.
pub(crate) async fn ensure_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let rootfs_path = default_rootfs_path(workspace_root, arch)?;
    ensure_managed_rootfs(workspace_root, arch, &rootfs_path).await?;
    Ok(rootfs_path)
}

/// Ensures a named managed rootfs image exists in the workspace cache.
///
/// This function resolves the image through the shared TGOS image storage.
async fn ensure_rootfs_image(workspace_root: &Path, image_name: &str) -> anyhow::Result<PathBuf> {
    let config = ImageConfig::read_config(workspace_root)?;
    let storage = Storage::new_from_config(&config).await?;
    ensure_rootfs_by_spec_from_storage(workspace_root, &storage, ImageSpecRef::parse(image_name))
        .await
}

pub(crate) async fn ensure_rootfs_by_spec_from_storage(
    workspace_root: &Path,
    storage: &Storage,
    spec: ImageSpecRef<'_>,
) -> anyhow::Result<PathBuf> {
    let image = storage.resolve_image(spec)?;
    if !is_managed_rootfs_image_name(&image.name) {
        bail!("image `{}` is not a managed rootfs image", image.name);
    }

    let rootfs_dir = rootfs_dir(workspace_root);
    let image_path = rootfs_dir.join(&image.name);

    tokio_fs::create_dir_all(&rootfs_dir)
        .await
        .with_context(|| format!("failed to create {}", rootfs_dir.display()))?;

    let _lock = crate::support::download::acquire_path_lock(&image_path).await?;
    if rootfs_image_source_matches(&image_path, &image.sha256)? {
        println!("rootfs image already exists and passed checksum verification");
        return Ok(image_path);
    }
    if rootfs_image_matches(&image_path, &image.sha256)? {
        write_rootfs_source_marker(&image_path, &image.sha256).await?;
        return Ok(image_path);
    }

    let extracted_dir = storage.pull_image(spec, None, true).await?;
    install_extracted_rootfs_image(&extracted_dir, image, &image_path).await?;
    write_rootfs_source_marker(&image_path, &image.sha256).await?;
    Ok(image_path)
}

fn rootfs_image_matches(image_path: &Path, expected_sha256: &str) -> anyhow::Result<bool> {
    if !image_path.exists() {
        return Ok(false);
    }

    match verify_file_sha256(image_path, expected_sha256) {
        Ok(true) => Ok(true),
        Ok(false) => {
            println!("existing rootfs image checksum mismatch, re-downloading...");
            Ok(false)
        }
        Err(err) => Err(err),
    }
}

fn rootfs_image_source_matches(image_path: &Path, expected_sha256: &str) -> anyhow::Result<bool> {
    if !image_path.exists() {
        return Ok(false);
    }

    let marker_path = rootfs_source_marker_path(image_path);
    let marker = match fs::read_to_string(&marker_path) {
        Ok(marker) => marker,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", marker_path.display()));
        }
    };
    Ok(marker.trim() == expected_sha256)
}

async fn write_rootfs_source_marker(image_path: &Path, sha256: &str) -> anyhow::Result<()> {
    let marker_path = rootfs_source_marker_path(image_path);
    tokio_fs::write(&marker_path, format!("{sha256}\n"))
        .await
        .with_context(|| format!("failed to write {}", marker_path.display()))
}

fn rootfs_source_marker_path(image_path: &Path) -> PathBuf {
    let mut file_name = image_path
        .file_name()
        .expect("rootfs image path must have a file name")
        .to_os_string();
    file_name.push(ROOTFS_SOURCE_SHA256_SUFFIX);
    image_path.with_file_name(file_name)
}

async fn remove_existing_file(path: &Path) -> anyhow::Result<()> {
    match tokio_fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn temporary_rootfs_image_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .expect("rootfs image path must have a file name")
        .to_os_string();
    file_name.push(".tmp");
    path.with_file_name(file_name)
}

async fn install_extracted_rootfs_image(
    extract_dir: &Path,
    image: &ImageEntry,
    image_path: &Path,
) -> anyhow::Result<()> {
    let source = find_extracted_rootfs_image(extract_dir, &image.name)?;
    let temp_dest = temporary_rootfs_image_path(image_path);
    remove_existing_file(&temp_dest).await?;
    tokio_fs::copy(&source, &temp_dest).await.with_context(|| {
        format!(
            "failed to copy extracted rootfs {} to {}",
            source.display(),
            temp_dest.display()
        )
    })?;
    tokio_fs::rename(&temp_dest, image_path)
        .await
        .with_context(|| {
            format!(
                "failed to move extracted rootfs {} to {}",
                temp_dest.display(),
                image_path.display()
            )
        })?;
    Ok(())
}

fn find_extracted_rootfs_image(extract_dir: &Path, image_name: &str) -> anyhow::Result<PathBuf> {
    let mut stack = vec![extract_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read extracted image dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) == Some(image_name) {
                return Ok(path);
            }
        }
    }

    bail!(
        "extracted image dir {} did not contain expected rootfs image `{image_name}`",
        extract_dir.display()
    )
}

fn is_managed_rootfs_image_name(name: &str) -> bool {
    name.starts_with("rootfs-") && name.ends_with(".img")
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;
    use crate::{image::config::DEFAULT_REGISTRY_URL, support::download::test_support};

    static REGISTRY_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    #[tokio::test]
    async fn ensure_rootfs_for_arch_skips_download_when_image_sha256_matches() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let workspace = tempdir().unwrap();
        let rootfs_dir = rootfs_dir(workspace.path());
        fs::create_dir_all(&rootfs_dir).unwrap();
        let image_name = "rootfs-loongarch64-alpine.img";
        let image_bytes = b"rootfs";
        fs::write(rootfs_dir.join(image_name), image_bytes).unwrap();
        let sha256 = sha256_hex(image_bytes);
        let registry = TestRegistry::register(image_name, &sha256, b"unused".to_vec());
        write_test_config(workspace.path());

        let image_path = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap();

        assert_eq!(fs::read(&image_path).unwrap(), b"rootfs");
        assert_eq!(
            fs::read_to_string(rootfs_source_marker_path(&image_path))
                .unwrap()
                .trim(),
            sha256
        );
        assert_eq!(registry.archive_request_count(), 0);
    }

    #[tokio::test]
    async fn ensure_rootfs_for_arch_skips_download_when_source_marker_matches() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let workspace = tempdir().unwrap();
        let rootfs_dir = rootfs_dir(workspace.path());
        fs::create_dir_all(&rootfs_dir).unwrap();
        let image_name = "rootfs-loongarch64-alpine.img";
        let image_path = rootfs_dir.join(image_name);
        fs::write(&image_path, b"locally patched rootfs").unwrap();
        let sha256 = sha256_hex(b"original rootfs");
        fs::write(
            rootfs_source_marker_path(&image_path),
            format!("{sha256}\n"),
        )
        .unwrap();
        let registry = TestRegistry::register(image_name, &sha256, b"unused".to_vec());
        write_test_config(workspace.path());

        let actual_path = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap();

        assert_eq!(actual_path, image_path);
        assert_eq!(fs::read(&actual_path).unwrap(), b"locally patched rootfs");
        assert_eq!(registry.archive_request_count(), 0);
    }

    #[tokio::test]
    async fn ensure_rootfs_for_arch_redownloads_when_image_sha256_mismatches() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let workspace = tempdir().unwrap();
        let rootfs_dir = rootfs_dir(workspace.path());
        fs::create_dir_all(&rootfs_dir).unwrap();
        let image_name = "rootfs-loongarch64-alpine.img";
        fs::write(rootfs_dir.join(image_name), b"old-rootfs").unwrap();
        let archive = make_tar_xz(&[(image_name, b"new-rootfs")]);
        let sha256 = sha256_hex(&archive);
        let registry = TestRegistry::register(image_name, &sha256, archive);
        write_test_config(workspace.path());

        let image_path = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap();

        assert_eq!(fs::read(&image_path).unwrap(), b"new-rootfs");
        assert_eq!(registry.archive_request_count(), 1);
    }

    #[tokio::test]
    async fn ensure_rootfs_for_arch_accepts_archive_sha256_registry_entry() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let workspace = tempdir().unwrap();
        let image_name = "rootfs-loongarch64-alpine.img";
        let archive = make_tar_xz(&[(image_name, b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let registry = TestRegistry::register(image_name, &sha256, archive);
        write_test_config(workspace.path());

        let image_path = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap();

        assert_eq!(fs::read(&image_path).unwrap(), b"rootfs");
        assert_eq!(
            fs::read_to_string(rootfs_source_marker_path(&image_path))
                .unwrap()
                .trim(),
            sha256
        );
        assert_eq!(registry.archive_request_count(), 1);
    }

    #[tokio::test]
    async fn ensure_managed_rootfs_uses_requested_image_name() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let image_name = "rootfs-aarch64-debian.img";
        let archive = make_tar_xz(&[(image_name, b"debian")]);
        let sha256 = sha256_hex(&archive);
        let registry = TestRegistry::register(image_name, &sha256, archive);
        let workspace = tempdir().unwrap();
        write_test_config(workspace.path());
        let rootfs_path = rootfs_dir(workspace.path()).join(image_name);

        ensure_managed_rootfs(workspace.path(), "aarch64", &rootfs_path)
            .await
            .unwrap();

        assert_eq!(fs::read(&rootfs_path).unwrap(), b"debian");
        assert_eq!(registry.archive_request_count(), 1);
    }

    #[tokio::test]
    async fn ensure_rootfs_for_arch_requires_registry_entry() {
        let _guard = REGISTRY_TEST_LOCK.lock().await;
        let workspace = tempdir().unwrap();
        write_test_config(workspace.path());
        test_support::register_text_url(DEFAULT_REGISTRY_URL, b"images = []\n".to_vec());

        let err = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("image not found: rootfs-loongarch64-alpine.img")
        );
        assert!(
            !rootfs_dir(workspace.path())
                .join("rootfs-loongarch64-alpine.img")
                .exists()
        );
    }

    fn write_test_config(workspace_root: &Path) {
        let config = ImageConfig {
            local_storage: workspace_root.join("image-cache"),
            registry: DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        ImageConfig::write_config(workspace_root, &config).unwrap();
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn make_tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
        use tar::Builder;
        use xz2::write::XzEncoder;

        let encoder = XzEncoder::new(Vec::new(), 6);
        let mut builder = Builder::new(encoder);
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_mode(0o644);
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    struct TestRegistry {
        archive: test_support::MockHandle,
    }

    impl TestRegistry {
        fn register(image_name: &str, sha256: &str, archive: Vec<u8>) -> Self {
            let archive =
                test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
            let registry = format!(
                r#"
[[images]]
name = "{image_name}"
version = "0.0.1"
released_at = "2026-01-01T00:00:00Z"
description = "test rootfs"
sha256 = "{sha256}"
arch = "test"
url = "{}"
"#,
                archive.url()
            );
            test_support::register_text_url(DEFAULT_REGISTRY_URL, registry.into_bytes());
            Self { archive }
        }

        fn archive_request_count(&self) -> usize {
            self.archive.request_count()
        }
    }
}

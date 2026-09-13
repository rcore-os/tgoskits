use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use flate2::read::GzDecoder;
use indicatif::ProgressBar;
use tar::Archive;
use xz2::read::XzDecoder;

use super::{
    config::ImageConfig,
    registry::{ImageEntry, ImageRegistry},
    spec::ImageSpecRef,
};
use crate::support::download::{
    DownloadOutcome, acquire_path_lock, download_file_verified_sha256, http_client,
};

pub const REGISTRY_FILENAME: &str = "images.toml";

#[derive(Debug)]
pub struct Storage {
    download_dir: PathBuf,
    extract_dir: PathBuf,
    pub image_registry: ImageRegistry,
}

impl Storage {
    pub async fn new_from_config(config: &ImageConfig) -> anyhow::Result<Self> {
        fs::create_dir_all(&config.download_dir)
            .with_context(|| format!("failed to create {}", config.download_dir.display()))?;
        fs::create_dir_all(&config.extract_dir)
            .with_context(|| format!("failed to create {}", config.extract_dir.display()))?;

        let client = http_client()?;
        println!("syncing image registry from {}...", config.registry);
        let image_registry = ImageRegistry::fetch_with_includes(&client, &config.registry).await?;
        let registry_filepath = registry_filepath(&config.download_dir);
        let toml_content = toml::to_string_pretty(&image_registry)
            .map_err(|e| anyhow!("Failed to serialize registry: {e}"))?;
        fs::write(&registry_filepath, toml_content)
            .map_err(|e| anyhow!("Failed to write registry file: {e}"))?;

        Ok(Self {
            download_dir: config.download_dir.clone(),
            extract_dir: config.extract_dir.clone(),
            image_registry,
        })
    }

    pub async fn pull_image(
        &self,
        spec: ImageSpecRef<'_>,
        extract: bool,
    ) -> anyhow::Result<PathBuf> {
        let image = self.resolve_image(spec)?;
        let archive_path = self.download_dir.join(image_archive_filename(image, spec));
        let archive_outcome = self.ensure_archive(image, &archive_path).await?;

        if !extract {
            println!("image archive ready at {}", archive_path.display());
            return Ok(archive_path);
        }

        let extract_dir = self.extract_dir.join(image_extract_dir_name(spec));
        if archive_outcome == DownloadOutcome::Reused && extract_dir.is_dir() {
            println!(
                "image archive is unchanged; keeping extracted image at {}",
                extract_dir.display()
            );
            return Ok(extract_dir);
        }

        let _lock = acquire_path_lock(&extract_dir).await?;
        if archive_outcome == DownloadOutcome::Reused && extract_dir.is_dir() {
            return Ok(extract_dir);
        }
        extract_archive(&archive_path, &extract_dir).await?;
        println!("image extracted to {}", extract_dir.display());
        Ok(extract_dir)
    }

    pub async fn pull_rootfs_image(&self, spec: ImageSpecRef<'_>) -> anyhow::Result<PathBuf> {
        let image = self.resolve_image(spec)?;
        ensure_rootfs_image_name(&image.name)?;
        let archive_path = self.download_dir.join(image_archive_filename(image, spec));
        let archive_outcome = self.ensure_archive(image, &archive_path).await?;
        let rootfs_path = self.extract_dir.join(&image.name);
        let _lock = acquire_path_lock(&rootfs_path).await?;

        if archive_outcome == DownloadOutcome::Reused && rootfs_path.is_file() {
            println!(
                "image archive is unchanged; keeping rootfs at {}",
                rootfs_path.display()
            );
            return Ok(rootfs_path);
        }

        extract_rootfs_archive(&archive_path, &rootfs_path, &image.name).await?;
        println!("image extracted to {}", rootfs_path.display());
        Ok(rootfs_path)
    }

    pub(crate) fn resolve_image<'a>(
        &'a self,
        spec: ImageSpecRef<'_>,
    ) -> anyhow::Result<&'a ImageEntry> {
        self.image_registry.find(spec).ok_or_else(|| {
            anyhow!(
                "image not found: {}. Use `cargo xtask image ls` to view available images",
                spec
            )
        })
    }

    async fn ensure_archive(
        &self,
        image: &ImageEntry,
        archive_path: &Path,
    ) -> anyhow::Result<DownloadOutcome> {
        let client = http_client()?;
        let outcome =
            download_file_verified_sha256(&client, &image.url, archive_path, &image.sha256).await?;
        println!("image archive verified at {}", archive_path.display());
        Ok(outcome)
    }
}

/// Returns the default managed rootfs image filename for a given architecture.
pub(crate) fn default_rootfs_image(arch: &str) -> Option<&'static str> {
    crate::context::default_rootfs_image_for_arch(arch)
}

/// Returns the directory containing mutable, extracted rootfs images.
pub(crate) fn rootfs_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    Ok(ImageConfig::read_config(workspace_root)?.extract_dir)
}

/// Resolves a QEMU rootfs reference into the configured extraction directory.
///
/// Checked-in QEMU configs use the default workspace rootfs directory as a
/// portable reference. A configured extraction directory replaces that prefix.
pub(crate) fn resolve_managed_rootfs_path(
    workspace_root: &Path,
    path: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let path = resolve_workspace_path(workspace_root, path);
    let rootfs_dir = rootfs_dir(workspace_root)?;
    let default_rootfs_dir = crate::context::axbuild_tmp_dir(workspace_root).join("rootfs");
    if !path.starts_with(&rootfs_dir) && !path.starts_with(&default_rootfs_dir) {
        return Ok(None);
    }

    let image_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid managed rootfs path `{}`", path.display()))?;
    ensure_rootfs_image_name(image_name)?;
    rootfs_image_path(workspace_root, image_name).map(Some)
}

/// Resolves a user-facing rootfs argument into the image storage path.
///
/// Bare values such as `alpine` or `debian` are expanded into the managed
/// `rootfs-<arch>-<distro>.img` naming scheme. Paths with a directory component
/// are treated as explicit user-managed paths.
pub(crate) fn resolve_rootfs_path(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> anyhow::Result<PathBuf> {
    let is_bare = rootfs
        .parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true);

    if !is_bare {
        return Ok(rootfs);
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

    rootfs_image_path(workspace_root, &image_name)
}

pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> anyhow::Result<PathBuf> {
    resolve_rootfs_path(workspace_root, arch, rootfs)
}

pub(crate) fn default_rootfs_path(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let image_name = default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
    rootfs_image_path(workspace_root, image_name)
}

pub(crate) async fn ensure_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let image_name = default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
    let storage = Storage::new_from_config(&ImageConfig::read_config(workspace_root)?).await?;
    storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
}

pub(crate) async fn ensure_managed_rootfs(
    workspace_root: &Path,
    arch: &str,
    path: &Path,
) -> anyhow::Result<()> {
    if default_rootfs_image(arch).is_none() {
        return Ok(());
    }

    let Some(path) = resolve_managed_rootfs_path(workspace_root, path)? else {
        return Ok(());
    };

    let image_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid managed rootfs path `{}`", path.display()))?;
    ensure_rootfs_image_name(image_name)?;
    let storage = Storage::new_from_config(&ImageConfig::read_config(workspace_root)?).await?;
    // A managed rootfs that is not a registry image but already exists locally was
    // produced on-host (e.g. by a Starry app `prebuild.sh` that bakes its own
    // rootfs into the canonical image-storage path). Accept the prepared file
    // as-is; only registry-backed images are (re)pulled. A non-registry image that
    // is missing locally still falls through to the error path below.
    if storage
        .resolve_image(ImageSpecRef::parse(image_name))
        .is_err()
        && path.is_file()
    {
        return Ok(());
    }
    let prepared = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await?;
    if prepared != path {
        bail!(
            "managed rootfs path mismatch: requested {}, prepared {}",
            path.display(),
            prepared.display()
        );
    }
    Ok(())
}

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

pub(crate) fn image_archive_filename(image: &ImageEntry, spec: ImageSpecRef<'_>) -> String {
    archive_filename_from_url(&image.url).unwrap_or_else(|| match spec.version {
        Some(version) => format!("{}-{}.tar.gz", spec.name, version),
        None => format!("{}.tar.gz", spec.name),
    })
}

pub(crate) fn image_extract_dir_name(spec: ImageSpecRef<'_>) -> String {
    match spec.version {
        Some(version) => format!("{}-{}", spec.name, version),
        None => spec.name.to_string(),
    }
}

fn registry_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(REGISTRY_FILENAME)
}

async fn extract_archive(archive_path: &Path, extract_dir: &Path) -> anyhow::Result<()> {
    if extract_dir.exists() {
        if extract_dir.is_dir() {
            fs::remove_dir_all(extract_dir)
        } else {
            fs::remove_file(extract_dir)
        }
        .with_context(|| format!("failed to remove {}", extract_dir.display()))?;
    }
    fs::create_dir_all(extract_dir)
        .with_context(|| format!("failed to create {}", extract_dir.display()))?;

    let archive_path = archive_path.to_path_buf();
    let extract_dir = extract_dir.to_path_buf();
    let archive_path_for_task = archive_path.clone();
    let extract_dir_for_task = extract_dir.clone();
    let progress = ProgressBar::new_spinner();
    progress.set_message(format!("extracting {}", archive_path.display()));
    progress.enable_steady_tick(std::time::Duration::from_millis(100));

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut archive_file = fs::File::open(&archive_path_for_task)
            .with_context(|| format!("failed to open {}", archive_path_for_task.display()))?;
        unpack_archive(
            &archive_path_for_task,
            &mut archive_file,
            &extract_dir_for_task,
        )?;
        Ok(())
    })
    .await
    .context("extract task failed")?;

    match result {
        Ok(()) => {
            progress.finish_with_message(format!("extracted {}", extract_dir.display()));
            Ok(())
        }
        Err(err) => {
            progress.abandon_with_message(format!("failed to extract {}", archive_path.display()));
            let _ = fs::remove_dir_all(extract_dir);
            Err(err)
        }
    }
}

async fn extract_rootfs_archive(
    archive_path: &Path,
    rootfs_path: &Path,
    image_name: &str,
) -> anyhow::Result<()> {
    let archive_path = archive_path.to_path_buf();
    let rootfs_path = rootfs_path.to_path_buf();
    let image_name = image_name.to_string();
    let progress = ProgressBar::new_spinner();
    progress.set_message(format!("extracting {}", archive_path.display()));
    progress.enable_steady_tick(std::time::Duration::from_millis(100));

    let archive_path_for_task = archive_path.clone();
    let rootfs_path_for_task = rootfs_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let extract_parent = rootfs_path_for_task.parent().ok_or_else(|| {
            anyhow!(
                "rootfs output path has no parent: {}",
                rootfs_path_for_task.display()
            )
        })?;
        let temp_dir = tempfile::Builder::new()
            .prefix(".rootfs-extract-")
            .tempdir_in(extract_parent)
            .with_context(|| {
                format!(
                    "failed to create temporary extraction directory in {}",
                    extract_parent.display()
                )
            })?;
        let mut archive_file = fs::File::open(&archive_path_for_task)
            .with_context(|| format!("failed to open {}", archive_path_for_task.display()))?;
        unpack_archive(&archive_path_for_task, &mut archive_file, temp_dir.path())?;
        let extracted_rootfs = find_extracted_rootfs_image(temp_dir.path(), &image_name)?;
        replace_extracted_rootfs(&extracted_rootfs, &rootfs_path_for_task)
    })
    .await
    .context("rootfs extraction task failed")?;

    match result {
        Ok(()) => {
            progress.finish_with_message(format!("extracted {}", rootfs_path.display()));
            Ok(())
        }
        Err(err) => {
            progress.abandon_with_message(format!("failed to extract {}", archive_path.display()));
            Err(err)
        }
    }
}

fn replace_extracted_rootfs(extracted_rootfs: &Path, rootfs_path: &Path) -> anyhow::Result<()> {
    match fs::rename(extracted_rootfs, rootfs_path) {
        Ok(()) => Ok(()),
        Err(_) if rootfs_path.exists() => {
            if rootfs_path.is_dir() {
                fs::remove_dir_all(rootfs_path)
            } else {
                fs::remove_file(rootfs_path)
            }
            .with_context(|| format!("failed to remove {}", rootfs_path.display()))?;
            fs::rename(extracted_rootfs, rootfs_path).with_context(|| {
                format!(
                    "failed to move extracted rootfs to {}",
                    rootfs_path.display()
                )
            })
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to move extracted rootfs to {}",
                rootfs_path.display()
            )
        }),
    }
}

fn archive_filename_from_url(url: &str) -> Option<String> {
    let path = url.split_once('?').map_or(url, |(path, _)| path);
    let name = path.rsplit('/').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn unpack_archive(
    archive_path: &Path,
    archive_file: &mut fs::File,
    extract_dir: &Path,
) -> anyhow::Result<()> {
    let mut magic = [0_u8; 6];
    let read_len = archive_file
        .read(&mut magic)
        .with_context(|| format!("failed to read {}", archive_path.display()))?;
    use std::io::{Seek, SeekFrom};
    archive_file
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", archive_path.display()))?;

    if read_len >= 2 && magic[..2] == [0x1f, 0x8b] {
        let decoder = GzDecoder::new(archive_file);
        let mut archive = Archive::new(decoder);
        return archive
            .unpack(extract_dir)
            .with_context(|| format!("failed to extract into {}", extract_dir.display()));
    }

    if read_len >= 6 && magic == [0xfd, b'7', b'z', b'X', b'Z', 0x00] {
        let decoder = XzDecoder::new(archive_file);
        let mut archive = Archive::new(decoder);
        return archive
            .unpack(extract_dir)
            .with_context(|| format!("failed to extract into {}", extract_dir.display()));
    }

    let mut archive = Archive::new(archive_file);
    archive
        .unpack(extract_dir)
        .with_context(|| format!("failed to extract into {}", extract_dir.display()))
}

fn rootfs_image_path(workspace_root: &Path, image_name: &str) -> anyhow::Result<PathBuf> {
    ensure_rootfs_image_name(image_name)?;
    let config = ImageConfig::read_config(workspace_root)?;
    Ok(config.extract_dir.join(image_name))
}

fn resolve_workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("${workspace}/") {
        return workspace_root.join(rest);
    }
    path.to_path_buf()
}

fn ensure_rootfs_image_name(image_name: &str) -> anyhow::Result<()> {
    if image_name.starts_with("rootfs-") && image_name.ends_with(".img") {
        return Ok(());
    }
    bail!("image `{image_name}` is not a managed rootfs image")
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

#[cfg(test)]
mod tests;

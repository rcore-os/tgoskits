use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, anyhow, bail};
use flate2::read::GzDecoder;
use indicatif::ProgressBar;
use tar::Archive;
use xz2::read::XzDecoder;

use super::{
    config::{ImageConfig, fallback_registry_url},
    registry::{ImageEntry, ImageRegistry},
    spec::ImageSpecRef,
};
use crate::support::download::{download_file_verified_sha256, http_client};

pub const REGISTRY_FILENAME: &str = "images.toml";
const LAST_SYNC_FILENAME: &str = ".last_sync";
const EXTRACTED_SHA256_FILENAME: &str = ".archive.sha256";

#[derive(Debug)]
pub struct Storage {
    pub path: PathBuf,
    pub image_registry: ImageRegistry,
}

impl Storage {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        let registry_filepath = registry_filepath(&path);
        let image_registry = ImageRegistry::load_from_file(&registry_filepath)?;
        Ok(Self {
            path,
            image_registry,
        })
    }

    pub async fn new_from_registry(registry: String, path: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&path).map_err(|e| anyhow!("Failed to create directory: {e}"))?;
        let client = http_client()?;
        let source =
            ImageRegistry::resolve_bootstrap_source(&client, &registry, &fallback_registry_url())
                .await?;
        println!(
            "bootstrapping local image registry from {}: {}",
            source.kind, source.url
        );
        let image_registry = ImageRegistry::fetch_with_includes(&client, &source.url).await?;
        Self::write_registry_to_path(path, image_registry)
    }

    fn write_registry_to_path(
        path: PathBuf,
        image_registry: ImageRegistry,
    ) -> anyhow::Result<Self> {
        let registry_filepath = registry_filepath(&path);
        let toml_content = toml::to_string_pretty(&image_registry)
            .map_err(|e| anyhow!("Failed to serialize registry: {e}"))?;
        fs::write(&registry_filepath, toml_content)
            .map_err(|e| anyhow!("Failed to write registry file: {e}"))?;
        write_last_sync_time(&path)?;
        Ok(Self {
            path,
            image_registry,
        })
    }

    pub async fn new_with_auto_sync(
        path: PathBuf,
        registry: String,
        auto_sync_threshold: u64,
    ) -> anyhow::Result<Self> {
        let storage = match Self::new(path.clone()) {
            Ok(storage) => storage,
            Err(err) => {
                println!("error while loading local storage: {err}");
                println!("auto syncing from registry {registry}...");
                return Self::new_from_registry(registry, path).await;
            }
        };

        if auto_sync_threshold == 0 {
            return Ok(storage);
        }

        let now = current_unix_timestamp()?;
        let last_sync = read_last_sync_time(&storage.path);
        let need_sync = match last_sync {
            None => true,
            Some(ts) => now.saturating_sub(ts) >= auto_sync_threshold,
        };
        if !need_sync {
            return Ok(storage);
        }

        let registry_path = registry_filepath(&storage.path);
        let backup = fs::read_to_string(&registry_path)
            .with_context(|| format!("Failed to read {}", registry_path.display()))?;
        match Self::new_from_registry(registry, path).await {
            Ok(storage) => Ok(storage),
            Err(err) => {
                println!("auto sync failed: {err}");
                fs::write(&registry_path, backup)
                    .with_context(|| format!("Failed to restore {}", registry_path.display()))?;
                Self::new(storage.path)
            }
        }
    }

    pub async fn new_from_config(config: &ImageConfig) -> anyhow::Result<Self> {
        if config.auto_sync {
            Self::new_with_auto_sync(
                config.local_storage.clone(),
                config.registry.clone(),
                config.auto_sync_threshold,
            )
            .await
        } else {
            Self::new(config.local_storage.clone())
        }
    }

    pub async fn pull_image(
        &self,
        spec: ImageSpecRef<'_>,
        output_dir: Option<&Path>,
        extract: bool,
    ) -> anyhow::Result<PathBuf> {
        let output_dir = output_dir.unwrap_or(&self.path);
        let image = self.resolve_image(spec)?;
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;

        let archive_path = output_dir.join(image_archive_filename(image, spec));
        self.ensure_archive(image, &archive_path).await?;

        if !extract {
            println!("image archive ready at {}", archive_path.display());
            return Ok(archive_path);
        }

        let extract_dir = output_dir.join(image_extract_dir_name(spec));
        if extracted_archive_matches(&extract_dir, &image.sha256)? {
            println!(
                "image already extracted and up to date at {}",
                extract_dir.display()
            );
            return Ok(extract_dir);
        }

        extract_archive(&archive_path, &extract_dir, &image.sha256).await?;
        println!("image extracted to {}", extract_dir.display());
        Ok(extract_dir)
    }

    pub async fn pull_rootfs_image(&self, spec: ImageSpecRef<'_>) -> anyhow::Result<PathBuf> {
        let image = self.resolve_image(spec)?;
        ensure_rootfs_image_name(&image.name)?;
        let extract_dir = self.pull_image(spec, None, true).await?;
        find_extracted_rootfs_image(&extract_dir, &image.name)
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

    async fn ensure_archive(&self, image: &ImageEntry, archive_path: &Path) -> anyhow::Result<()> {
        let client = http_client()?;
        download_file_verified_sha256(&client, &image.url, archive_path, &image.sha256).await?;
        println!("image archive verified at {}", archive_path.display());
        Ok(())
    }

    #[cfg(test)]
    async fn new_with_auto_sync_for_test(
        path: PathBuf,
        auto_sync_threshold: u64,
        image_registry: ImageRegistry,
    ) -> anyhow::Result<Self> {
        let storage = match Self::new(path.clone()) {
            Ok(storage) => storage,
            Err(_) => return Self::write_registry_to_path(path, image_registry),
        };

        if auto_sync_threshold == 0 {
            return Ok(storage);
        }

        let now = current_unix_timestamp()?;
        let last_sync = read_last_sync_time(&storage.path);
        let need_sync = match last_sync {
            None => true,
            Some(ts) => now.saturating_sub(ts) >= auto_sync_threshold,
        };
        if !need_sync {
            return Ok(storage);
        }

        Self::write_registry_to_path(path, image_registry)
    }
}

/// Returns the default managed rootfs image filename for a given architecture.
pub(crate) fn default_rootfs_image(arch: &str) -> Option<&'static str> {
    crate::context::default_rootfs_image_for_arch(arch)
}

/// Returns the local storage directory used for image-managed files.
pub(crate) fn rootfs_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    Ok(ImageConfig::read_config(workspace_root)?.local_storage)
}

/// Resolves a QEMU rootfs path into the canonical image storage path.
///
/// Checked-in QEMU configs may still refer to the historical
/// `tmp/axbuild/rootfs/rootfs-*.img` location. That path is treated only as a
/// reference to an image-managed rootfs; the actual file remains in image
/// storage.
pub(crate) fn resolve_managed_rootfs_path(
    workspace_root: &Path,
    path: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let path = resolve_workspace_path(workspace_root, path);
    let rootfs_dir = rootfs_dir(workspace_root)?;
    let legacy_rootfs_dir = crate::context::axbuild_tmp_dir(workspace_root).join("rootfs");
    if !path.starts_with(&rootfs_dir) && !path.starts_with(&legacy_rootfs_dir) {
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

fn last_sync_filepath(storage_path: &Path) -> PathBuf {
    storage_path.join(LAST_SYNC_FILENAME)
}

fn current_unix_timestamp() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow!("System time error: {e}"))
        .map(|d| d.as_secs())
}

fn read_last_sync_time(storage_path: &Path) -> Option<u64> {
    let path = last_sync_filepath(storage_path);
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse::<u64>().ok()
}

fn write_last_sync_time(storage_path: &Path) -> anyhow::Result<()> {
    let now = current_unix_timestamp()?;
    fs::write(last_sync_filepath(storage_path), now.to_string())
        .map_err(|e| anyhow!("Failed to write last sync file: {e}"))
}

fn extracted_archive_matches(extract_dir: &Path, expected_sha256: &str) -> anyhow::Result<bool> {
    if !extract_dir.exists() {
        return Ok(false);
    }
    if !extract_dir.is_dir() {
        return Ok(false);
    }

    let marker_path = extract_dir.join(EXTRACTED_SHA256_FILENAME);
    let actual_sha256 = match fs::read_to_string(&marker_path) {
        Ok(actual_sha256) => actual_sha256,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(anyhow!(
                "failed to read extraction marker {}: {err}",
                marker_path.display()
            ));
        }
    };

    Ok(actual_sha256.trim() == expected_sha256)
}

async fn extract_archive(
    archive_path: &Path,
    extract_dir: &Path,
    expected_sha256: &str,
) -> anyhow::Result<()> {
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
    let expected_sha256 = expected_sha256.to_string();
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
        fs::write(
            extract_dir_for_task.join(EXTRACTED_SHA256_FILENAME),
            expected_sha256,
        )
        .with_context(|| {
            format!(
                "failed to write extraction marker in {}",
                extract_dir_for_task.display()
            )
        })?;
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
    let spec = ImageSpecRef::parse(image_name);
    Ok(config
        .local_storage
        .join(image_extract_dir_name(spec))
        .join(image_name))
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

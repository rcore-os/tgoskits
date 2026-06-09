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
        fs::remove_dir_all(extract_dir)
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
mod tests {
    use std::io::Write;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;
    use crate::{image::registry::RegistrySource, support::download::test_support};

    fn sample_registry() -> &'static str {
        r#"
[[images]]
name = "linux"
version = "0.0.1"
released_at = "2025-01-01T00:00:00Z"
description = "Linux guest"
sha256 = "abc"
arch = "aarch64"
url = "https://example.com/linux-0.0.1.tar.gz"
"#
    }

    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_data);
            for (name, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, *contents).unwrap();
            }
            builder.finish().unwrap();
        }

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_data).unwrap();
        encoder.finish().unwrap()
    }

    fn make_tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
        let mut builder = tar::Builder::new(encoder);
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn image_entry(name: &str, version: &str, url: &str) -> ImageEntry {
        ImageEntry {
            name: name.to_string(),
            version: version.to_string(),
            released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
            description: "Linux guest".to_string(),
            sha256: "abc".to_string(),
            arch: "aarch64".to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn names_follow_registry_url_with_default_fallback() {
        let xz_image = image_entry("linux", "0.0.1", "https://example.com/linux.tar.xz");
        assert_eq!(
            image_archive_filename(&xz_image, ImageSpecRef::parse("linux")),
            "linux.tar.xz"
        );

        let fallback_image = image_entry("linux", "0.0.1", "https://example.com/");
        assert_eq!(
            image_archive_filename(&fallback_image, ImageSpecRef::parse("linux")),
            "linux.tar.gz"
        );
        assert_eq!(
            image_archive_filename(&fallback_image, ImageSpecRef::parse("linux:0.0.1")),
            "linux-0.0.1.tar.gz"
        );
        assert_eq!(
            image_extract_dir_name(ImageSpecRef::parse("linux")),
            "linux"
        );
        assert_eq!(
            image_extract_dir_name(ImageSpecRef::parse("linux:0.0.1")),
            "linux-0.0.1"
        );
    }

    #[test]
    fn loads_local_registry() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join(REGISTRY_FILENAME), sample_registry()).unwrap();

        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

        assert_eq!(storage.image_registry.images.len(), 1);
        assert_eq!(storage.image_registry.images[0].name, "linux");
    }

    #[tokio::test]
    async fn auto_sync_fetches_registry_when_missing() {
        let dir = tempdir().unwrap();
        let sample = dir.path().join("sample.toml");
        fs::write(&sample, sample_registry()).unwrap();
        let image_registry = ImageRegistry::load_from_file(&sample).unwrap();

        let storage =
            Storage::new_with_auto_sync_for_test(dir.path().to_path_buf(), 60, image_registry)
                .await
                .unwrap();

        assert_eq!(storage.image_registry.images.len(), 1);
        assert!(dir.path().join(REGISTRY_FILENAME).exists());
    }

    #[tokio::test]
    async fn pull_image_skips_reextract_when_marker_matches() {
        let dir = tempdir().unwrap();
        let image_name = "linux";
        let archive_bytes = make_tar_gz(&[("kernel.bin", b"kernel")]);
        let sha256 = sha256_hex(&archive_bytes);
        let registry_path = dir.path().join(REGISTRY_FILENAME);
        fs::write(
            &registry_path,
            format!(
                r#"
[[images]]
name = "{image_name}"
version = "0.0.1"
released_at = "2025-01-01T00:00:00Z"
description = "Linux guest"
sha256 = "{sha256}"
arch = "aarch64"
url = "https://example.com/{image_name}.tar.gz"
"#
            ),
        )
        .unwrap();
        fs::write(
            dir.path().join(image_archive_filename(
                &image_entry(
                    image_name,
                    "0.0.1",
                    format!("https://example.com/{image_name}.tar.gz").as_str(),
                ),
                ImageSpecRef::parse(image_name),
            )),
            archive_bytes,
        )
        .unwrap();
        let extract_dir = dir
            .path()
            .join(image_extract_dir_name(ImageSpecRef::parse(image_name)));
        fs::create_dir_all(&extract_dir).unwrap();
        fs::write(extract_dir.join(EXTRACTED_SHA256_FILENAME), &sha256).unwrap();
        fs::write(extract_dir.join("sentinel"), b"keep").unwrap();

        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let extracted = storage
            .pull_image(ImageSpecRef::parse(image_name), None, true)
            .await
            .unwrap();

        assert_eq!(extracted, extract_dir);
        assert_eq!(fs::read(extract_dir.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn config_without_auto_sync_requires_local_registry() {
        let dir = tempdir().unwrap();
        let config = ImageConfig {
            local_storage: dir.path().to_path_buf(),
            registry: "https://example.com/registry.toml".to_string(),
            auto_sync: false,
            auto_sync_threshold: 60,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(Storage::new_from_config(&config)).unwrap_err();

        assert!(err.to_string().contains("Failed to read image registry"));
    }

    #[test]
    fn resolve_managed_rootfs_path_accepts_legacy_tmp_rootfs_reference() {
        let workspace = tempdir().unwrap();
        let image_name = "rootfs-aarch64-busybox.img";
        let config = ImageConfig {
            local_storage: workspace.path().join(".tgos-images"),
            registry: "https://example.com/registry.toml".to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        ImageConfig::write_config(workspace.path(), &config).unwrap();

        let legacy_path = workspace.path().join("tmp/axbuild/rootfs").join(image_name);
        let resolved = resolve_managed_rootfs_path(workspace.path(), &legacy_path).unwrap();

        assert_eq!(
            resolved,
            Some(config.local_storage.join(image_name).join(image_name))
        );
    }

    #[test]
    fn resolve_managed_rootfs_path_accepts_workspace_legacy_reference() {
        let workspace = tempdir().unwrap();
        let image_name = "rootfs-aarch64-busybox.img";
        let config = ImageConfig {
            local_storage: workspace.path().join(".tgos-images"),
            registry: "https://example.com/registry.toml".to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        ImageConfig::write_config(workspace.path(), &config).unwrap();

        let legacy_path = PathBuf::from(format!("${{workspace}}/tmp/axbuild/rootfs/{image_name}"));
        let resolved = resolve_managed_rootfs_path(workspace.path(), &legacy_path).unwrap();

        assert_eq!(
            resolved,
            Some(config.local_storage.join(image_name).join(image_name))
        );
    }

    #[tokio::test]
    async fn pull_downloads_and_extracts_image() {
        let archive = make_tar_gz(&[
            ("rootfs.img", b"rootfs"),
            ("qemu-aarch64", b"kernel"),
            ("axvm-bios.bin", b"bios"),
        ]);
        let sha256 = sha256_hex(&archive);
        let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());

        let dir = tempdir().unwrap();
        let registry = ImageRegistry {
            images: vec![ImageEntry {
                name: "qemu_x86_64_nimbos".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "NimbOS guest".to_string(),
                sha256,
                arch: "x86_64".to_string(),
                url: archive_url.url().to_string(),
            }],
        };
        fs::write(
            dir.path().join(REGISTRY_FILENAME),
            toml::to_string(&registry).unwrap(),
        )
        .unwrap();

        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let extracted = storage
            .pull_image(ImageSpecRef::parse("qemu_x86_64_nimbos"), None, true)
            .await
            .unwrap();

        assert_eq!(extracted, dir.path().join("qemu_x86_64_nimbos"));
        assert_eq!(fs::read(extracted.join("rootfs.img")).unwrap(), b"rootfs");
        assert!(dir.path().join("archive.tar.gz").exists());
        assert!(!dir.path().join("archive.tar.gz.part").exists());
    }

    #[tokio::test]
    async fn pull_downloads_and_extracts_xz_image() {
        let archive = make_tar_xz(&[("rootfs.img", b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url = test_support::register_bytes("rootfs.img.tar.xz", archive.clone());
        let dir = tempdir().unwrap();
        let storage = Storage {
            path: dir.path().to_path_buf(),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: "rootfs-riscv64-alpine.img".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Alpine rootfs".to_string(),
                    sha256,
                    arch: "riscv64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        let extracted = storage
            .pull_image(ImageSpecRef::parse("rootfs-riscv64-alpine.img"), None, true)
            .await
            .unwrap();

        assert_eq!(fs::read(extracted.join("rootfs.img")).unwrap(), b"rootfs");
        assert!(dir.path().join("rootfs.img.tar.xz").exists());
    }

    #[tokio::test]
    async fn pull_rootfs_image_returns_extracted_rootfs_file() {
        let image_name = "rootfs-riscv64-alpine.img";
        let archive = make_tar_xz(&[(image_name, b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url =
            test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
        let dir = tempdir().unwrap();
        let storage = Storage {
            path: dir.path().to_path_buf(),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: image_name.to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Alpine rootfs".to_string(),
                    sha256,
                    arch: "riscv64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        let rootfs = storage
            .pull_rootfs_image(ImageSpecRef::parse(image_name))
            .await
            .unwrap();

        assert_eq!(rootfs, dir.path().join(image_name).join(image_name));
        assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
        assert_eq!(archive_url.request_count(), 1);
    }

    #[tokio::test]
    async fn pull_rootfs_image_skips_download_when_archive_matches() {
        let image_name = "rootfs-riscv64-alpine.img";
        let archive = make_tar_xz(&[(image_name, b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url =
            test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive.clone());
        let dir = tempdir().unwrap();
        let storage = Storage {
            path: dir.path().to_path_buf(),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: image_name.to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Alpine rootfs".to_string(),
                    sha256,
                    arch: "riscv64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        let rootfs = storage
            .pull_rootfs_image(ImageSpecRef::parse(image_name))
            .await
            .unwrap();
        fs::write(&rootfs, b"patched rootfs").unwrap();
        let rootfs_again = storage
            .pull_rootfs_image(ImageSpecRef::parse(image_name))
            .await
            .unwrap();

        assert_eq!(rootfs_again, rootfs);
        assert_eq!(fs::read(rootfs_again).unwrap(), b"patched rootfs");
        assert_eq!(archive_url.request_count(), 1);
    }

    #[tokio::test]
    async fn ensure_rootfs_for_arch_uses_image_storage_path() {
        let image_name = "rootfs-loongarch64-alpine.img";
        let archive = make_tar_xz(&[(image_name, b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url =
            test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
        let workspace = tempdir().unwrap();
        let config = ImageConfig {
            local_storage: workspace.path().join("image-cache"),
            registry: "https://example.com/registry.toml".to_string(),
            auto_sync: false,
            auto_sync_threshold: 60,
        };
        ImageConfig::write_config(workspace.path(), &config).unwrap();
        let registry = ImageRegistry {
            images: vec![ImageEntry {
                name: image_name.to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Alpine rootfs".to_string(),
                sha256,
                arch: "loongarch64".to_string(),
                url: archive_url.url().to_string(),
            }],
        };
        fs::create_dir_all(&config.local_storage).unwrap();
        fs::write(
            config.local_storage.join(REGISTRY_FILENAME),
            toml::to_string(&registry).unwrap(),
        )
        .unwrap();

        let rootfs = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
            .await
            .unwrap();

        assert_eq!(
            rootfs,
            config.local_storage.join(image_name).join(image_name)
        );
        assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
    }

    #[tokio::test]
    async fn pull_redownloads_when_existing_archive_is_invalid() {
        let archive = make_tar_gz(&[("rootfs.img", b"new-rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
        let dir = tempdir().unwrap();
        let storage = Storage {
            path: dir.path().to_path_buf(),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: "linux".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Linux guest".to_string(),
                    sha256,
                    arch: "aarch64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        fs::write(dir.path().join("linux.tar.gz"), b"corrupt").unwrap();
        let extracted = storage
            .pull_image(ImageSpecRef::parse("linux"), None, true)
            .await
            .unwrap();

        assert_eq!(
            fs::read(extracted.join("rootfs.img")).unwrap(),
            b"new-rootfs"
        );
    }

    #[tokio::test]
    async fn pull_uses_custom_output_dir() {
        let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
        let root = tempdir().unwrap();
        let output = root.path().join("images");
        let storage = Storage {
            path: root.path().join("default"),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: "linux".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Linux guest".to_string(),
                    sha256,
                    arch: "aarch64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        let extracted = storage
            .pull_image(ImageSpecRef::parse("linux"), Some(&output), true)
            .await
            .unwrap();

        assert_eq!(extracted, output.join("linux"));
        assert!(output.join("archive.tar.gz").exists());
        assert_eq!(
            fs::read(output.join("linux/rootfs.img")).unwrap(),
            b"rootfs"
        );
    }

    #[tokio::test]
    async fn failed_checksum_does_not_leave_final_or_part_file() {
        let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
        let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
        let dir = tempdir().unwrap();
        let storage = Storage {
            path: dir.path().to_path_buf(),
            image_registry: ImageRegistry {
                images: vec![ImageEntry {
                    name: "linux".to_string(),
                    version: "0.0.1".to_string(),
                    released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                    description: "Linux guest".to_string(),
                    sha256: "deadbeef".to_string(),
                    arch: "aarch64".to_string(),
                    url: archive_url.url().to_string(),
                }],
            },
        };

        let err = storage
            .pull_image(ImageSpecRef::parse("linux"), None, false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("checksum mismatch"));
        assert!(!dir.path().join("linux.tar.gz").exists());
        assert!(!dir.path().join("linux.tar.gz.part").exists());
    }

    #[tokio::test]
    async fn bootstrap_source_falls_back_when_default_is_unavailable() {
        let fallback_body = br#"
[[images]]
name = "linux"
version = "0.0.1"
description = "Linux guest"
sha256 = "abc"
arch = "aarch64"
        url = "https://example.com/linux.tar.gz"
        "#
        .to_vec();
        let fallback = test_support::register_text("fallback.toml", fallback_body);
        let client = http_client().unwrap();
        let source = ImageRegistry::resolve_bootstrap_source(
            &client,
            "mock://missing/default.toml",
            fallback.url(),
        )
        .await
        .unwrap();

        assert_eq!(
            source,
            RegistrySource {
                url: fallback.url().to_string(),
                kind: "fallback registry",
            }
        );
    }

    #[tokio::test]
    async fn bootstrap_source_prefers_include_from_default() {
        let default_body = br#"
[[includes]]
url = "http://127.0.0.1:0/included.toml"
"#
        .to_vec();
        let default = test_support::register_text("default.toml", default_body);
        let client = http_client().unwrap();
        let source = ImageRegistry::resolve_bootstrap_source(
            &client,
            default.url(),
            "mock://missing/fallback.toml",
        )
        .await
        .unwrap();

        assert_eq!(source.kind, "included registry from default.toml");
        assert_eq!(source.url, "http://127.0.0.1:0/included.toml");
    }
}

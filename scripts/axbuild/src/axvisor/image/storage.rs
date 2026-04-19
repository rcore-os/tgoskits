use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use flate2::read::GzDecoder;
use indicatif::ProgressBar;
use sha2::{Digest, Sha256};
use tar::Archive;

use super::{
    config::{ImageConfig, fallback_registry_url},
    registry::{ImageEntry, ImageRegistry},
    spec::ImageSpecRef,
};
use crate::download::{download_to_path_with_progress, http_client};

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

        let archive_path = output_dir.join(image_archive_filename(spec));
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

    fn resolve_image<'a>(&'a self, spec: ImageSpecRef<'_>) -> anyhow::Result<&'a ImageEntry> {
        self.image_registry.find(spec).ok_or_else(|| {
            anyhow!(
                "image not found: {}. Use `cargo axvisor image ls` to view available images",
                spec
            )
        })
    }

    async fn ensure_archive(&self, image: &ImageEntry, archive_path: &Path) -> anyhow::Result<()> {
        if archive_path.exists() {
            match image_verify_sha256(archive_path, &image.sha256) {
                Ok(true) => {
                    println!("image already exists and passed checksum verification");
                    return Ok(());
                }
                Ok(false) => {
                    println!("existing image checksum mismatch, re-downloading...");
                }
                Err(err) => {
                    println!("failed to verify existing image: {err}, re-downloading...");
                }
            }
            fs::remove_file(archive_path)
                .with_context(|| format!("failed to remove {}", archive_path.display()))?;
        }

        let part_path = part_path_for(archive_path);
        if part_path.exists() {
            fs::remove_file(&part_path)
                .with_context(|| format!("failed to remove {}", part_path.display()))?;
        }

        let client = http_client()?;
        let download_result = download_to_path_with_progress(&client, &image.url, &part_path).await;
        if let Err(err) = download_result {
            let _ = fs::remove_file(&part_path);
            return Err(err);
        }

        match image_verify_sha256(&part_path, &image.sha256) {
            Ok(true) => {}
            Ok(false) => {
                let _ = fs::remove_file(&part_path);
                bail!("downloaded image checksum mismatch for {}", image.url);
            }
            Err(err) => {
                let _ = fs::remove_file(&part_path);
                return Err(err);
            }
        }

        fs::rename(&part_path, archive_path).with_context(|| {
            format!(
                "failed to move downloaded archive {} to {}",
                part_path.display(),
                archive_path.display()
            )
        })?;
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

pub(crate) fn image_archive_filename(spec: ImageSpecRef<'_>) -> String {
    match spec.version {
        Some(version) => format!("{}-{}.tar.gz", spec.name, version),
        None => format!("{}.tar.gz", spec.name),
    }
}

pub(crate) fn image_extract_dir_name(spec: ImageSpecRef<'_>) -> String {
    match spec.version {
        Some(version) => format!("{}-{}", spec.name, version),
        None => spec.name.to_string(),
    }
}

fn part_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.part"))
        .unwrap_or_else(|| "download.part".to_string());
    path.with_file_name(name)
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

fn image_verify_sha256(file_path: &Path, expected_sha256: &str) -> anyhow::Result<bool> {
    let mut file = fs::File::open(file_path)
        .with_context(|| format!("failed to open {}", file_path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let actual_sha256 = format!("{:x}", hasher.finalize());
    Ok(actual_sha256 == expected_sha256)
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
        let archive_file = fs::File::open(&archive_path_for_task)
            .with_context(|| format!("failed to open {}", archive_path_for_task.display()))?;
        let decoder = GzDecoder::new(archive_file);
        let mut archive = Archive::new(decoder);
        archive.unpack(&extract_dir_for_task).with_context(|| {
            format!("failed to extract into {}", extract_dir_for_task.display())
        })?;
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

#[cfg(test)]
mod tests {
    use std::{io::Write, net::SocketAddr, sync::Arc};

    use tempfile::tempdir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    use super::*;
    use crate::axvisor::image::registry::RegistrySource;

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

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    async fn start_http_server(
        routes: Vec<(String, Vec<u8>)>,
    ) -> (SocketAddr, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let (tx, mut rx) = oneshot::channel();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accept = listener.accept() => {
                        let (mut stream, _) = accept.unwrap();
                        let routes = routes.clone();
                        tokio::spawn(async move {
                            let mut buf = [0u8; 4096];
                            let size = stream.read(&mut buf).await.unwrap();
                            let request = String::from_utf8_lossy(&buf[..size]);
                            let path = request
                                .lines()
                                .next()
                                .and_then(|line| line.split_whitespace().nth(1))
                                .unwrap_or("/");
                            let response = routes
                                .iter()
                                .find(|(route, _)| route == path)
                                .map(|(_, body)| {
                                    format!(
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    )
                                    .into_bytes()
                                });
                            if let Some(mut header) = response {
                                let body = routes
                                    .iter()
                                    .find(|(route, _)| route == path)
                                    .map(|(_, body)| body.clone())
                                    .unwrap();
                                header.extend_from_slice(&body);
                                let _ = stream.write_all(&header).await;
                            } else {
                                let _ = stream
                                    .write_all(
                                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                    )
                                    .await;
                            }
                        });
                    }
                }
            }
        });

        (addr, tx)
    }

    #[test]
    fn names_follow_legacy_layout() {
        assert_eq!(
            image_archive_filename(ImageSpecRef::parse("linux")),
            "linux.tar.gz"
        );
        assert_eq!(
            image_archive_filename(ImageSpecRef::parse("linux:0.0.1")),
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
            dir.path()
                .join(image_archive_filename(ImageSpecRef::parse(image_name))),
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

    #[tokio::test]
    async fn pull_downloads_and_extracts_image() {
        let archive = make_tar_gz(&[
            ("rootfs.img", b"rootfs"),
            ("qemu-aarch64", b"kernel"),
            ("axvm-bios.bin", b"bios"),
        ]);
        let sha256 = sha256_hex(&archive);
        let (addr, shutdown) =
            start_http_server(vec![("/archive.tar.gz".to_string(), archive.clone())]).await;

        let dir = tempdir().unwrap();
        let registry = ImageRegistry {
            images: vec![ImageEntry {
                name: "qemu_x86_64_nimbos".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "NimbOS guest".to_string(),
                sha256,
                arch: "x86_64".to_string(),
                url: format!("http://{addr}/archive.tar.gz"),
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
        assert!(dir.path().join("qemu_x86_64_nimbos.tar.gz").exists());
        assert!(!dir.path().join("qemu_x86_64_nimbos.tar.gz.part").exists());
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn pull_redownloads_when_existing_archive_is_invalid() {
        let archive = make_tar_gz(&[("rootfs.img", b"new-rootfs")]);
        let sha256 = sha256_hex(&archive);
        let (addr, shutdown) =
            start_http_server(vec![("/archive.tar.gz".to_string(), archive.clone())]).await;
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
                    url: format!("http://{addr}/archive.tar.gz"),
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
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn pull_uses_custom_output_dir() {
        let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
        let sha256 = sha256_hex(&archive);
        let (addr, shutdown) =
            start_http_server(vec![("/archive.tar.gz".to_string(), archive.clone())]).await;
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
                    url: format!("http://{addr}/archive.tar.gz"),
                }],
            },
        };

        let extracted = storage
            .pull_image(ImageSpecRef::parse("linux"), Some(&output), true)
            .await
            .unwrap();

        assert_eq!(extracted, output.join("linux"));
        assert!(output.join("linux.tar.gz").exists());
        assert_eq!(
            fs::read(output.join("linux/rootfs.img")).unwrap(),
            b"rootfs"
        );
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn failed_checksum_does_not_leave_final_or_part_file() {
        let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
        let (addr, shutdown) =
            start_http_server(vec![("/archive.tar.gz".to_string(), archive.clone())]).await;
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
                    url: format!("http://{addr}/archive.tar.gz"),
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
        let _ = shutdown.send(());
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
        let (addr, shutdown) =
            start_http_server(vec![("/fallback.toml".to_string(), fallback_body)]).await;
        let client = http_client().unwrap();
        let source = ImageRegistry::resolve_bootstrap_source(
            &client,
            "http://127.0.0.1:9/default.toml",
            &format!("http://{addr}/fallback.toml"),
        )
        .await
        .unwrap();

        assert_eq!(
            source,
            RegistrySource {
                url: format!("http://{addr}/fallback.toml"),
                kind: "fallback registry",
            }
        );
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn bootstrap_source_prefers_include_from_default() {
        let default_body = br#"
[[includes]]
url = "http://127.0.0.1:0/included.toml"
"#
        .to_vec();
        let (addr, shutdown) =
            start_http_server(vec![("/default.toml".to_string(), default_body)]).await;
        let client = http_client().unwrap();
        let source = ImageRegistry::resolve_bootstrap_source(
            &client,
            &format!("http://{addr}/default.toml"),
            "http://127.0.0.1:9/fallback.toml",
        )
        .await
        .unwrap();

        assert_eq!(source.kind, "included registry from default.toml");
        assert_eq!(source.url, "http://127.0.0.1:0/included.toml");
        let _ = shutdown.send(());
    }
}

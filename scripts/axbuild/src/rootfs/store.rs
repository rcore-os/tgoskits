//! Managed rootfs image storage and retrieval helpers.
//!
//! Main responsibilities:
//! - Define default naming rules for workspace-managed rootfs images
//! - Resolve user-facing `--rootfs` values into concrete image paths
//! - Manage image files and cached archives under `target/rootfs/`
//! - Download and extract rootfs archives on demand so images are available
//!   locally

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use flate2::read::GzDecoder;
use tokio::fs as tokio_fs;

const TGOSIMAGES_ROOTFS_RELEASE: &str = "v0.0.4";

/// Returns the default managed rootfs image filename for a given architecture.
pub(crate) fn default_rootfs_image(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("rootfs-aarch64-alpine.img"),
        "riscv64" => Some("rootfs-riscv64-alpine.img"),
        "x86_64" => Some("rootfs-x86_64-alpine.img"),
        "loongarch64" => Some("rootfs-loongarch64-alpine.img"),
        _ => None,
    }
}

/// Returns the workspace directory that stores managed rootfs images.
pub(crate) fn rootfs_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("rootfs")
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

/// Ensures the default managed rootfs image for an architecture is available.
pub(crate) async fn ensure_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let image_name = default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
    ensure_rootfs_image(workspace_root, image_name).await
}

/// Builds the release asset URL for a managed rootfs archive.
fn archive_url(image_name: &str) -> String {
    format!(
        "https://github.com/rcore-os/tgosimages/releases/download/{}/{}",
        TGOSIMAGES_ROOTFS_RELEASE,
        archive_name(image_name)
    )
}

/// Returns the managed archive filename for a rootfs image.
fn archive_name(image_name: &str) -> String {
    format!("{image_name}.tar.gz")
}

/// Returns the local cache path for a managed rootfs archive.
fn archive_path(workspace_root: &Path, image_name: &str) -> PathBuf {
    rootfs_dir(workspace_root).join(archive_name(image_name))
}

/// Ensures a named managed rootfs image exists in the workspace cache.
///
/// This function downloads the corresponding archive on demand and retries once
/// if extraction fails due to a corrupt cached archive.
async fn ensure_rootfs_image(workspace_root: &Path, image_name: &str) -> anyhow::Result<PathBuf> {
    let rootfs_dir = rootfs_dir(workspace_root);
    let image_path = rootfs_dir.join(image_name);

    if image_path.exists() {
        return Ok(image_path);
    }

    tokio_fs::create_dir_all(&rootfs_dir)
        .await
        .with_context(|| format!("failed to create {}", rootfs_dir.display()))?;

    let archive_path = archive_path(workspace_root, image_name);
    let client = crate::download::http_client()?;

    download_archive(&client, image_name, &archive_path).await?;
    if let Err(err) = extract_image(&archive_path, image_name, &rootfs_dir).await {
        if archive_path.exists() {
            eprintln!(
                "failed to extract managed rootfs archive {}, re-downloading: {err}",
                archive_path.display()
            );
            tokio_fs::remove_file(&archive_path)
                .await
                .with_context(|| format!("failed to remove {}", archive_path.display()))?;
            download_archive(&client, image_name, &archive_path).await?;
            extract_image(&archive_path, image_name, &rootfs_dir).await?;
        } else {
            return Err(err);
        }
    }

    Ok(image_path)
}

/// Downloads the archive for a managed rootfs image if it is not cached yet.
async fn download_archive(
    client: &reqwest::Client,
    image_name: &str,
    archive_path: &Path,
) -> anyhow::Result<()> {
    if archive_path.exists() {
        return Ok(());
    }

    println!(
        "managed rootfs archive not found, downloading from rcore-os/tgosimages release {}...",
        TGOSIMAGES_ROOTFS_RELEASE
    );
    crate::download::download_file(client, &archive_url(image_name), archive_path).await
}

/// Extracts a single rootfs image entry from an archive on a blocking worker.
async fn extract_image(
    archive_path: &Path,
    image_name: &str,
    out_dir: &Path,
) -> anyhow::Result<()> {
    let archive_path = archive_path.to_path_buf();
    let image_name = image_name.to_string();
    let out_dir = out_dir.to_path_buf();
    tokio::task::spawn_blocking(move || unpack_image(&archive_path, &image_name, &out_dir))
        .await
        .context("rootfs extraction task failed")?
}

/// Unpacks the expected rootfs image file from a `.tar.gz` archive.
fn unpack_image(archive_path: &Path, image_name: &str, out_dir: &Path) -> anyhow::Result<()> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    for entry in archive
        .entries()
        .with_context(|| format!("failed to read entries from {}", archive_path.display()))?
    {
        let mut entry = entry.with_context(|| "failed to read tarball entry")?;
        let raw_path = entry
            .path()
            .with_context(|| "failed to get tarball entry path")?
            .into_owned();
        let Some(name) = raw_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "." || raw_path.components().count() == 0 || name != image_name {
            continue;
        }
        let dest = out_dir.join(name);
        if !dest.exists() {
            entry
                .unpack(&dest)
                .with_context(|| format!("failed to extract `{name}` to {}", dest.display()))?;
        }
        return Ok(());
    }

    Err(anyhow!(
        "archive {} did not contain expected rootfs image `{image_name}`",
        archive_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn ensure_rootfs_for_arch_redownloads_invalid_cached_archive() {
        let archive = make_tar_gz(&[("rootfs-loongarch64-alpine.img", b"rootfs")]);
        let server = TestServer::start(archive).await;
        let workspace = tempdir().unwrap();
        let rootfs_dir = rootfs_dir(workspace.path());
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(
            rootfs_dir.join("rootfs-loongarch64-alpine.img.tar.gz"),
            b"corrupt",
        )
        .unwrap();

        let image_path =
            ensure_rootfs_for_arch_with_url(workspace.path(), "loongarch64", &server.url())
                .await
                .unwrap();

        assert_eq!(fs::read(&image_path).unwrap(), b"rootfs");
        assert_eq!(server.request_count(), 1);
    }

    #[tokio::test]
    async fn ensure_managed_rootfs_uses_requested_image_name() {
        let archive = make_tar_gz(&[("rootfs-aarch64-debian.img", b"debian")]);
        let server = TestServer::start(archive).await;
        let workspace = tempdir().unwrap();
        let rootfs_path = rootfs_dir(workspace.path()).join("rootfs-aarch64-debian.img");

        ensure_managed_rootfs_with_url(workspace.path(), "aarch64", &rootfs_path, &server.url())
            .await
            .unwrap();

        assert_eq!(fs::read(&rootfs_path).unwrap(), b"debian");
        assert_eq!(server.request_count(), 1);
    }

    async fn ensure_rootfs_for_arch_with_url(
        workspace_root: &Path,
        arch: &str,
        url: &str,
    ) -> anyhow::Result<PathBuf> {
        let image_name = default_rootfs_image(arch)
            .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
        ensure_rootfs_image_with_url(workspace_root, image_name, url).await
    }

    async fn ensure_managed_rootfs_with_url(
        workspace_root: &Path,
        arch: &str,
        path: &Path,
        url: &str,
    ) -> anyhow::Result<()> {
        if !path.starts_with(rootfs_dir(workspace_root)) {
            return Ok(());
        }
        if default_rootfs_image(arch).is_none() {
            return Ok(());
        }
        let image_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("invalid managed rootfs path `{}`", path.display()))?;
        ensure_rootfs_image_with_url(workspace_root, image_name, url).await?;
        Ok(())
    }

    async fn ensure_rootfs_image_with_url(
        workspace_root: &Path,
        image_name: &str,
        url: &str,
    ) -> anyhow::Result<PathBuf> {
        let rootfs_dir = rootfs_dir(workspace_root);
        let image_path = rootfs_dir.join(image_name);
        if image_path.exists() {
            return Ok(image_path);
        }

        tokio_fs::create_dir_all(&rootfs_dir).await?;
        let archive_path = archive_path(workspace_root, image_name);
        let client = crate::download::http_client()?;
        download_archive_with_url(&client, url, &archive_path).await?;

        if let Err(err) = extract_image(&archive_path, image_name, &rootfs_dir).await {
            if archive_path.exists() {
                tokio_fs::remove_file(&archive_path).await?;
                download_archive_with_url(&client, url, &archive_path).await?;
                extract_image(&archive_path, image_name, &rootfs_dir).await?;
            } else {
                return Err(err);
            }
        }

        Ok(image_path)
    }

    async fn download_archive_with_url(
        client: &reqwest::Client,
        url: &str,
        archive_path: &Path,
    ) -> anyhow::Result<()> {
        if archive_path.exists() {
            return Ok(());
        }
        crate::download::download_file(client, url, archive_path).await
    }

    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        use tar::Builder;

        let encoder = GzEncoder::new(Vec::new(), Compression::default());
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

    struct TestServer {
        addr: std::net::SocketAddr,
        requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl TestServer {
        async fn start(body: Vec<u8>) -> Self {
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_counter = requests.clone();
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        accepted = listener.accept() => {
                            let Ok((mut socket, _)) = accepted else {
                                break;
                            };
                            let body = body.clone();
                            let request_counter = request_counter.clone();
                            tokio::spawn(async move {
                                let mut buf = [0u8; 4096];
                                let _ = socket.read(&mut buf).await;
                                request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                let header = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                let _ = socket.write_all(header.as_bytes()).await;
                                let _ = socket.write_all(&body).await;
                            });
                        }
                    }
                }
            });

            Self {
                addr,
                requests,
                shutdown: Some(shutdown_tx),
            }
        }

        fn url(&self) -> String {
            format!("http://{}/rootfs.img.tar.gz", self.addr)
        }

        fn request_count(&self) -> usize {
            self.requests.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }
}

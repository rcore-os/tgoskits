use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, anyhow};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{StatusCode, header};
use tokio::{fs as tokio_fs, io::AsyncWriteExt};

const TGOSIMAGES_ROOTFS_RELEASE: &str = "v0.0.4";
const DOWNLOAD_LOCK_STALE_AFTER: Duration = Duration::from_secs(60 * 60 * 2);

/// Returns the default managed rootfs image shipped for the given arch.
pub(crate) fn default_rootfs_image(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("rootfs-aarch64-alpine.img"),
        "riscv64" => Some("rootfs-riscv64-alpine.img"),
        "x86_64" => Some("rootfs-x86_64-alpine.img"),
        "loongarch64" => Some("rootfs-loongarch64-alpine.img"),
        _ => None,
    }
}

pub(crate) fn rootfs_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("rootfs")
}

/// Resolves `--rootfs` values into a path under the managed rootfs directory.
///
/// Bare keywords such as `alpine`, `busybox`, and `debian` are expanded into
/// `rootfs-<arch>-<distro>.img`. Paths that already contain a directory
/// component are left untouched.
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

/// Ensures a rootfs path under [`rootfs_dir`] exists locally before use.
///
/// Paths outside the managed directory are treated as user-managed and skipped.
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

/// Ensures the default rootfs image for `arch` is downloaded and extracted.
pub(crate) async fn ensure_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let image_name = default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for arch `{arch}`"))?;
    ensure_rootfs_image(workspace_root, image_name).await
}

fn archive_url(image_name: &str) -> String {
    format!(
        "https://github.com/rcore-os/tgosimages/releases/download/{}/{}",
        TGOSIMAGES_ROOTFS_RELEASE,
        archive_name(image_name)
    )
}

fn archive_name(image_name: &str) -> String {
    format!("{image_name}.tar.gz")
}

fn archive_path(workspace_root: &Path, image_name: &str) -> PathBuf {
    rootfs_dir(workspace_root).join(archive_name(image_name))
}

/// Makes sure a specific managed rootfs image exists, re-downloading its
/// archive if extraction fails due to a corrupt local cache.
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
    let client = http_client()?;

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
    download_file(client, &archive_url(image_name), archive_path).await
}

/// Extracts a single rootfs image out of a `.tar.gz` archive on a blocking
/// worker thread so the async runtime stays responsive.
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

/// Unpacks only the requested image entry from the archive instead of
/// extracting every file in it.
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

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| anyhow!("failed to create HTTP client: {e}"))
}

pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))
}

/// Downloads a file with best-effort resume support via an adjacent `.part`
/// file. If the server ignores the `Range` request, the partial file is
/// truncated and the download restarts from the beginning.
pub(crate) async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
) -> anyhow::Result<()> {
    download_file_inner(client, url, path, true).await
}

async fn download_file_inner(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    retry_on_invalid_range: bool,
) -> anyhow::Result<()> {
    let _lock = acquire_path_lock(path).await?;
    let part_path = part_path(path);
    let resume_from = tokio_fs::metadata(&part_path)
        .await
        .map(|meta| meta.len())
        .or_else(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Ok(0)
            } else {
                Err(err)
            }
        })
        .with_context(|| format!("failed to read metadata for {}", part_path.display()))?;

    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header(header::RANGE, format!("bytes={resume_from}-"));
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?;

    let status = response.status();
    if retry_on_invalid_range && resume_from > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
        drop(_lock);
        tokio_fs::remove_file(&part_path)
            .await
            .with_context(|| format!("failed to remove invalid partial {}", part_path.display()))?;
        return Box::pin(download_file_inner(client, url, path, false)).await;
    }

    let resume = resume_from > 0 && status == StatusCode::PARTIAL_CONTENT;
    if !resume && resume_from > 0 && status == StatusCode::OK {
        println!(
            "server did not honor range request for {}, restarting download from scratch",
            path.display()
        );
    }

    if !status.is_success() {
        return Err(anyhow!("failed to download {url}: HTTP {status}"));
    }

    let initial_size = if resume { resume_from } else { 0 };
    let total_size = response
        .content_length()
        .map(|content_length| content_length + initial_size);

    let progress = progress_bar(total_size, path);
    if initial_size > 0 {
        progress.set_position(initial_size);
        progress.set_message(format!("resuming download {}", path.display()));
    }
    let mut file = tokio_fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!resume)
        .append(resume)
        .open(&part_path)
        .await
        .with_context(|| format!("failed to open {}", part_path.display()))?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed while downloading {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("failed to write {}", part_path.display()))?;
        progress.inc(chunk.len() as u64);
    }

    file.flush()
        .await
        .with_context(|| format!("failed to flush {}", part_path.display()))?;
    drop(file);
    tokio_fs::rename(&part_path, path).await.with_context(|| {
        format!(
            "failed to move downloaded file {} to {}",
            part_path.display(),
            path.display()
        )
    })?;
    progress.finish_with_message(format!("downloaded {}", path.display()));
    Ok(())
}

/// Returns the companion lock path used to serialize concurrent downloads of
/// the same target.
fn lock_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .expect("download output path must have a file name")
        .to_os_string();
    file_name.push(".lock");
    path.with_file_name(file_name)
}

/// Returns the companion partial-download path used for resumable transfers.
fn part_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .expect("download output path must have a file name")
        .to_os_string();
    file_name.push(".part");
    path.with_file_name(file_name)
}

async fn acquire_path_lock(path: &Path) -> anyhow::Result<PathLock> {
    let lock_path = lock_path(path);

    loop {
        match tokio_fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .await
        {
            Ok(_) => return Ok(PathLock { path: lock_path }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if stale_lock(&lock_path).await.unwrap_or(false) {
                    let _ = tokio_fs::remove_file(&lock_path).await;
                    continue;
                }
                tokio::task::yield_now().await;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to acquire lock {}", lock_path.display()));
            }
        }
    }
}

async fn stale_lock(path: &Path) -> anyhow::Result<bool> {
    let modified = tokio_fs::metadata(path)
        .await
        .with_context(|| format!("failed to read metadata for {}", path.display()))?
        .modified()
        .with_context(|| format!("failed to read mtime for {}", path.display()))?;
    Ok(lock_age(modified).is_some_and(|age| age >= DOWNLOAD_LOCK_STALE_AFTER))
}

fn lock_age(modified: SystemTime) -> Option<Duration> {
    SystemTime::now().duration_since(modified).ok()
}

/// Builds either a byte-count progress bar or a spinner when the server does
/// not report content length.
fn progress_bar(total_size: Option<u64>, path: &Path) -> ProgressBar {
    match total_size {
        Some(total_size) => {
            let progress = ProgressBar::new(total_size);
            progress.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} \
                     ({bytes_per_sec}, ETA {eta})",
                )
                .expect("valid progress bar template")
                .progress_chars("##-"),
            );
            progress.set_message(format!("downloading {}", path.display()));
            progress
        }
        None => {
            let progress = ProgressBar::new_spinner();
            progress.set_message(format!("downloading {}", path.display()));
            progress.enable_steady_tick(std::time::Duration::from_millis(100));
            progress
        }
    }
}

struct PathLock {
    path: PathBuf,
}

impl Drop for PathLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn part_path_uses_dot_part_suffix() {
        let path = Path::new("/tmp/rootfs-x86_64-alpine.img.tar.gz");
        assert_eq!(
            part_path(path),
            PathBuf::from("/tmp/rootfs-x86_64-alpine.img.tar.gz.part")
        );
    }

    #[test]
    fn lock_path_uses_dot_lock_suffix() {
        let path = Path::new("/tmp/rootfs-x86_64-alpine.img.tar.gz");
        assert_eq!(
            lock_path(path),
            PathBuf::from("/tmp/rootfs-x86_64-alpine.img.tar.gz.lock")
        );
    }

    #[tokio::test]
    async fn download_file_resumes_partial_download() {
        let server = TestServer::start_with_range_support(b"abcdef".to_vec(), true).await;
        let workspace = tempdir().unwrap();
        let output_path = workspace.path().join("rootfs.img.tar.gz");
        let part_path = part_path(&output_path);
        fs::write(&part_path, b"abc").unwrap();

        let client = http_client().unwrap();
        download_file(&client, &server.url(), &output_path)
            .await
            .unwrap();

        assert_eq!(fs::read(&output_path).unwrap(), b"abcdef");
        assert_eq!(server.last_range_header().as_deref(), Some("bytes=3-"));
    }

    #[tokio::test]
    async fn download_file_restarts_when_range_is_ignored() {
        let server = TestServer::start_with_range_support(b"abcdef".to_vec(), false).await;
        let workspace = tempdir().unwrap();
        let output_path = workspace.path().join("rootfs.img.tar.gz");
        let part_path = part_path(&output_path);
        fs::write(&part_path, b"abc").unwrap();

        let client = http_client().unwrap();
        download_file(&client, &server.url(), &output_path)
            .await
            .unwrap();

        assert_eq!(fs::read(&output_path).unwrap(), b"abcdef");
        assert_eq!(server.last_range_header().as_deref(), Some("bytes=3-"));
    }

    #[tokio::test]
    async fn download_file_restarts_when_range_is_invalid() {
        let server = TestServer::start_with_invalid_range(b"abcdef".to_vec()).await;
        let workspace = tempdir().unwrap();
        let output_path = workspace.path().join("rootfs.img.tar.gz");
        let part_path = part_path(&output_path);
        fs::write(&part_path, b"abcdefghi").unwrap();

        let client = http_client().unwrap();
        download_file(&client, &server.url(), &output_path)
            .await
            .unwrap();

        assert_eq!(fs::read(&output_path).unwrap(), b"abcdef");
        assert_eq!(server.request_count(), 2);
    }

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
        let client = http_client()?;
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
        download_file(client, url, archive_path).await
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
        last_range_header: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl TestServer {
        async fn start(body: Vec<u8>) -> Self {
            Self::start_with_range_support(body, false).await
        }

        async fn start_with_invalid_range(body: Vec<u8>) -> Self {
            Self::start_inner(body, RangeMode::RejectInvalid).await
        }

        async fn start_with_range_support(body: Vec<u8>, support_range: bool) -> Self {
            let mode = if support_range {
                RangeMode::Support
            } else {
                RangeMode::Ignore
            };
            Self::start_inner(body, mode).await
        }

        async fn start_inner(body: Vec<u8>, range_mode: RangeMode) -> Self {
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_counter = requests.clone();
            let last_range_header = std::sync::Arc::new(std::sync::Mutex::new(None));
            let range_header_slot = last_range_header.clone();
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
                            let range_header_slot = range_header_slot.clone();
                            tokio::spawn(async move {
                                let mut buf = [0u8; 4096];
                                let read = socket.read(&mut buf).await.unwrap_or(0);
                                let request = String::from_utf8_lossy(&buf[..read]);
                                let range_header = request
                                    .lines()
                                    .find_map(|line| line.strip_prefix("Range: "))
                                    .map(str::trim)
                                    .map(ToOwned::to_owned);
                                *range_header_slot.lock().unwrap() = range_header.clone();
                                request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                let (status, response_body, extra_headers) = match (
                                    range_mode,
                                    range_header.as_deref(),
                                ) {
                                    (RangeMode::Support, Some(range_header)) => {
                                        let offset = range_header
                                            .strip_prefix("bytes=")
                                            .and_then(|value| value.strip_suffix('-'))
                                            .and_then(|value| value.parse::<usize>().ok())
                                            .unwrap_or(0);
                                        (
                                            "206 Partial Content",
                                            body[offset..].to_vec(),
                                            format!(
                                                "Content-Range: bytes {}-{}/{}\r\n",
                                                offset,
                                                body.len().saturating_sub(1),
                                                body.len()
                                            ),
                                        )
                                    }
                                    (RangeMode::RejectInvalid, Some(range_header)) => {
                                        let offset = range_header
                                            .strip_prefix("bytes=")
                                            .and_then(|value| value.strip_suffix('-'))
                                            .and_then(|value| value.parse::<usize>().ok())
                                            .unwrap_or(0);
                                        if offset >= body.len() {
                                            (
                                                "416 Range Not Satisfiable",
                                                Vec::new(),
                                                format!(
                                                    "Content-Range: bytes */{}\r\n",
                                                    body.len()
                                                ),
                                            )
                                        } else {
                                            (
                                                "206 Partial Content",
                                                body[offset..].to_vec(),
                                                format!(
                                                    "Content-Range: bytes {}-{}/{}\r\n",
                                                    offset,
                                                    body.len().saturating_sub(1),
                                                    body.len()
                                                ),
                                            )
                                        }
                                    }
                                    _ => ("200 OK", body.clone(), String::new()),
                                };
                                let header = format!(
                                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n{extra_headers}Connection: close\r\n\r\n",
                                    response_body.len()
                                );
                                let _ = socket.write_all(header.as_bytes()).await;
                                let _ = socket.write_all(&response_body).await;
                            });
                        }
                    }
                }
            });

            Self {
                addr,
                requests,
                last_range_header,
                shutdown: Some(shutdown_tx),
            }
        }

        fn url(&self) -> String {
            format!("http://{}/rootfs.img.tar.gz", self.addr)
        }

        fn request_count(&self) -> usize {
            self.requests.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn last_range_header(&self) -> Option<String> {
            self.last_range_header.lock().unwrap().clone()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    #[derive(Clone, Copy)]
    enum RangeMode {
        Ignore,
        Support,
        RejectInvalid,
    }
}

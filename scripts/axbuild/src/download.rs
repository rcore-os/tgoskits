use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, anyhow};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{StatusCode, header};
use tokio::{fs as tokio_fs, io::AsyncWriteExt};

const DOWNLOAD_LOCK_STALE_AFTER: Duration = Duration::from_secs(60 * 60 * 2);

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

fn lock_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .expect("download output path must have a file name")
        .to_os_string();
    file_name.push(".lock");
    path.with_file_name(file_name)
}

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
            Ok(file) => {
                write_lock_metadata(file, &lock_path).await?;
                return Ok(PathLock { path: lock_path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if recoverable_lock(&lock_path).await.unwrap_or(false) {
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

async fn write_lock_metadata(file: tokio_fs::File, lock_path: &Path) -> anyhow::Result<()> {
    let mut file = file.into_std().await;
    writeln!(file, "pid={}", std::process::id())
        .with_context(|| format!("failed to write lock metadata to {}", lock_path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush lock metadata to {}", lock_path.display()))?;
    Ok(())
}

async fn recoverable_lock(path: &Path) -> anyhow::Result<bool> {
    if let Some(pid) = read_lock_pid(path).await?
        && !process_is_running(pid)
    {
        return Ok(true);
    }

    stale_lock(path).await
}

async fn read_lock_pid(path: &Path) -> anyhow::Result<Option<u32>> {
    let contents = match tokio_fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read lock {}", path.display()));
        }
    };

    Ok(contents.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|pid| pid.trim().parse::<u32>().ok())
    }))
}

fn process_is_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        true
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
    async fn recoverable_lock_accepts_dead_process_pid() {
        let workspace = tempdir().unwrap();
        let lock_path = workspace.path().join("download.lock");
        fs::write(&lock_path, "pid=999999\n").unwrap();

        assert!(recoverable_lock(&lock_path).await.unwrap());
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

    struct TestServer {
        addr: std::net::SocketAddr,
        requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        last_range_header: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl TestServer {
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
                                                format!("Content-Range: bytes */{}\r\n", body.len()),
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

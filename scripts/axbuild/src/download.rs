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
    #[cfg(test)]
    if let Some(response) = test_support::fetch_text(url) {
        return response;
    }

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

    #[cfg(test)]
    if let Some(response) = test_support::download_response(url, resume_from) {
        let response = response?;
        let status = response.status;
        if retry_on_invalid_range && resume_from > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE
        {
            drop(_lock);
            tokio_fs::remove_file(&part_path).await.with_context(|| {
                format!("failed to remove invalid partial {}", part_path.display())
            })?;
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
            .content_length
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
        file.write_all(&response.body)
            .await
            .with_context(|| format!("failed to write {}", part_path.display()))?;
        progress.inc(response.body.len() as u64);
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
        return Ok(());
    }

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
pub(crate) mod test_support {
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
    };

    use reqwest::StatusCode;

    #[derive(Debug, Clone, Copy)]
    pub(crate) enum MockRangeMode {
        Ignore,
        Support,
        RejectInvalid,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct MockHandle {
        url: String,
        route: Arc<MockRoute>,
    }

    impl MockHandle {
        pub(crate) fn url(&self) -> &str {
            &self.url
        }

        pub(crate) fn request_count(&self) -> usize {
            self.route.requests.load(Ordering::SeqCst)
        }

        pub(crate) fn last_range_header(&self) -> Option<String> {
            self.route.last_range_header.lock().unwrap().clone()
        }
    }

    #[derive(Debug)]
    pub(super) struct MockResponse {
        pub(super) status: StatusCode,
        pub(super) content_length: Option<u64>,
        pub(super) body: Vec<u8>,
    }

    #[derive(Debug)]
    struct MockRoute {
        body: Vec<u8>,
        range_mode: MockRangeMode,
        requests: AtomicUsize,
        last_range_header: Mutex<Option<String>>,
    }

    static NEXT_ROUTE_ID: AtomicU64 = AtomicU64::new(0);
    static ROUTES: OnceLock<Mutex<HashMap<String, Arc<MockRoute>>>> = OnceLock::new();

    pub(crate) fn register_bytes(path: &str, body: Vec<u8>) -> MockHandle {
        register_download(path, body, MockRangeMode::Ignore)
    }

    pub(crate) fn register_text(path: &str, body: Vec<u8>) -> MockHandle {
        register_bytes(path, body)
    }

    pub(crate) fn register_download(
        path: &str,
        body: Vec<u8>,
        range_mode: MockRangeMode,
    ) -> MockHandle {
        let id = NEXT_ROUTE_ID.fetch_add(1, Ordering::SeqCst);
        let path = path.trim_start_matches('/');
        let url = format!("mock://axbuild-test/{id}/{path}");
        let route = Arc::new(MockRoute {
            body,
            range_mode,
            requests: AtomicUsize::new(0),
            last_range_header: Mutex::new(None),
        });
        routes().lock().unwrap().insert(url.clone(), route.clone());
        MockHandle { url, route }
    }

    pub(super) fn fetch_text(url: &str) -> Option<anyhow::Result<String>> {
        if !is_mock_url(url) {
            return None;
        }

        Some(route(url).and_then(|route| {
            route.requests.fetch_add(1, Ordering::SeqCst);
            *route.last_range_header.lock().unwrap() = None;
            String::from_utf8(route.body.clone())
                .map_err(|err| anyhow::anyhow!("mock response for {url} is not UTF-8: {err}"))
        }))
    }

    pub(super) fn download_response(
        url: &str,
        resume_from: u64,
    ) -> Option<anyhow::Result<MockResponse>> {
        if !is_mock_url(url) {
            return None;
        }

        Some(route(url).map(|route| {
            route.requests.fetch_add(1, Ordering::SeqCst);
            let range_header = (resume_from > 0).then(|| format!("bytes={resume_from}-"));
            *route.last_range_header.lock().unwrap() = range_header.clone();
            let offset = range_header
                .as_deref()
                .and_then(|header| header.strip_prefix("bytes="))
                .and_then(|value| value.strip_suffix('-'))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);

            match (route.range_mode, range_header.is_some()) {
                (MockRangeMode::Support, true) if offset < route.body.len() => {
                    let body = route.body[offset..].to_vec();
                    MockResponse {
                        status: StatusCode::PARTIAL_CONTENT,
                        content_length: Some(body.len() as u64),
                        body,
                    }
                }
                (MockRangeMode::Support | MockRangeMode::RejectInvalid, true)
                    if offset >= route.body.len() =>
                {
                    MockResponse {
                        status: StatusCode::RANGE_NOT_SATISFIABLE,
                        content_length: Some(0),
                        body: Vec::new(),
                    }
                }
                (MockRangeMode::RejectInvalid, true) => {
                    let body = route.body[offset..].to_vec();
                    MockResponse {
                        status: StatusCode::PARTIAL_CONTENT,
                        content_length: Some(body.len() as u64),
                        body,
                    }
                }
                _ => MockResponse {
                    status: StatusCode::OK,
                    content_length: Some(route.body.len() as u64),
                    body: route.body.clone(),
                },
            }
        }))
    }

    fn is_mock_url(url: &str) -> bool {
        url.starts_with("mock://")
    }

    fn route(url: &str) -> anyhow::Result<Arc<MockRoute>> {
        routes()
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("mock URL not registered: {url}"))
    }

    fn routes() -> &'static Mutex<HashMap<String, Arc<MockRoute>>> {
        ROUTES.get_or_init(|| Mutex::new(HashMap::new()))
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
        handle: test_support::MockHandle,
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
            let range_mode = match range_mode {
                RangeMode::Ignore => test_support::MockRangeMode::Ignore,
                RangeMode::Support => test_support::MockRangeMode::Support,
                RangeMode::RejectInvalid => test_support::MockRangeMode::RejectInvalid,
            };
            Self {
                handle: test_support::register_download("rootfs.img.tar.gz", body, range_mode),
            }
        }

        fn url(&self) -> String {
            self.handle.url().to_string()
        }

        fn request_count(&self) -> usize {
            self.handle.request_count()
        }

        fn last_range_header(&self) -> Option<String> {
            self.handle.last_range_header()
        }
    }

    #[derive(Clone, Copy)]
    enum RangeMode {
        Ignore,
        Support,
        RejectInvalid,
    }
}

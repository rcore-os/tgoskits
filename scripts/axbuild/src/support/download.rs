use std::{
    fmt, fs,
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, anyhow, bail};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{StatusCode, header};
use sha2::{Digest, Sha256};
use tokio::{fs as tokio_fs, io::AsyncWriteExt};

const DOWNLOAD_LOCK_STALE_AFTER: Duration = Duration::from_secs(60 * 60 * 2);
const DOWNLOAD_LOCK_WAIT: Duration = Duration::from_millis(100);
const DOWNLOAD_MAX_ATTEMPTS: usize = 5;
#[cfg(not(test))]
const DOWNLOAD_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const DOWNLOAD_RETRY_BASE_DELAY: Duration = Duration::from_millis(1);

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    http_client_builder()
        .build()
        .map_err(|e| anyhow!("failed to create HTTP client: {e}"))
}

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 30))
}

pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    with_download_retries(client, url, |client| async move {
        fetch_text_inner(&client, url).await
    })
    .await
}

async fn fetch_text_inner(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
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

#[cfg(test)]
pub(crate) async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
) -> anyhow::Result<()> {
    let _lock = acquire_path_lock(path).await?;
    download_file_with_retries(client, url, path).await
}

pub(crate) async fn download_file_verified_sha256(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    expected_sha256: &str,
) -> anyhow::Result<DownloadOutcome> {
    let _lock = acquire_path_lock(path).await?;
    if path.exists() {
        match file_sha256(path) {
            Ok(actual_sha256) if actual_sha256 == expected_sha256 => {
                println!("file already exists and passed checksum verification");
                return Ok(DownloadOutcome::Reused);
            }
            Ok(_) => {
                println!("existing file checksum mismatch, re-downloading...");
            }
            Err(err) => {
                println!("failed to verify existing file: {err}, re-downloading...");
            }
        }
        tokio_fs::remove_file(path)
            .await
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }

    download_file_with_retries(client, url, path).await?;
    let actual_sha256 = file_sha256(path)?;
    if actual_sha256 != expected_sha256 {
        let _ = tokio_fs::remove_file(path).await;
        bail!(
            "downloaded file checksum mismatch for {url}: expected {expected_sha256}, got \
             {actual_sha256}"
        );
    }

    Ok(DownloadOutcome::Downloaded)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadOutcome {
    Reused,
    Downloaded,
}

async fn download_file_with_retries(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
) -> anyhow::Result<()> {
    with_download_retries(client, url, |client| async move {
        download_file_inner(&client, url, path, true).await
    })
    .await
}

async fn with_download_retries<T, F, Fut>(
    client: &reqwest::Client,
    url: &str,
    mut download: F,
) -> anyhow::Result<T>
where
    F: FnMut(reqwest::Client) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut http1_client = None;
    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        match download(http1_client.as_ref().unwrap_or(client).clone()).await {
            Ok(result) => return Ok(result),
            Err(err) if attempt < DOWNLOAD_MAX_ATTEMPTS && retryable_download_error(&err) => {
                if http1_client.is_none() && http2_protocol_error(&err) {
                    // Repeating a refused stream on HTTP/2 may never recover. Use
                    // HTTP/1.1 for the remaining attempts without resetting the budget.
                    http1_client = Some(
                        http_client_builder()
                            .http1_only()
                            .build()
                            .context("failed to create HTTP/1.1 download client")?,
                    );
                    eprintln!("HTTP/2 download failed for {url}; switching to HTTP/1.1");
                }
                let delay = download_retry_delay(attempt);
                eprintln!(
                    "download attempt {attempt}/{DOWNLOAD_MAX_ATTEMPTS} for {url} failed: \
                     {err:#}; retrying in {:.1}s",
                    delay.as_secs_f32()
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("download retry loop always returns")
}

pub(crate) fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

async fn download_file_inner(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    retry_on_invalid_range: bool,
) -> anyhow::Result<()> {
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
            return Err(download_status_error(url, status));
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
        return Err(download_status_error(url, status));
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

#[derive(Debug)]
struct DownloadStatusError {
    url: String,
    status: StatusCode,
}

impl fmt::Display for DownloadStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to download {}: HTTP {}", self.url, self.status)
    }
}

impl std::error::Error for DownloadStatusError {}

fn download_status_error(url: &str, status: StatusCode) -> anyhow::Error {
    DownloadStatusError {
        url: url.to_owned(),
        status,
    }
    .into()
}

fn retryable_download_error(err: &anyhow::Error) -> bool {
    http2_protocol_error(err)
        || err.chain().any(|cause| {
            cause
                .downcast_ref::<DownloadStatusError>()
                .is_some_and(|err| retryable_status(err.status))
                || cause.downcast_ref::<reqwest::Error>().is_some_and(|err| {
                    err.status().is_some_and(retryable_status)
                        || err.is_timeout()
                        || err.is_connect()
                        || err.is_request()
                        || err.is_body()
                })
        })
}

fn http2_protocol_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<h2::Error>()
            .is_some_and(|error| error.reason().is_some())
    })
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn download_retry_delay(attempt: usize) -> Duration {
    DOWNLOAD_RETRY_BASE_DELAY * (1 << attempt.saturating_sub(1).min(3))
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

pub(crate) async fn acquire_path_lock(path: &Path) -> anyhow::Result<PathLock> {
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
                tokio::time::sleep(DOWNLOAD_LOCK_WAIT).await;
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

pub(crate) struct PathLock {
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
        collections::{HashMap, VecDeque},
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
        failing_statuses: Mutex<VecDeque<StatusCode>>,
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
        register_download_with_failures(path, body, range_mode, Vec::new())
    }

    pub(crate) fn register_download_with_failures(
        path: &str,
        body: Vec<u8>,
        range_mode: MockRangeMode,
        failing_statuses: Vec<StatusCode>,
    ) -> MockHandle {
        let id = NEXT_ROUTE_ID.fetch_add(1, Ordering::SeqCst);
        let path = path.trim_start_matches('/');
        let url = format!("mock://axbuild-test/{id}/{path}");
        let route = Arc::new(MockRoute {
            body,
            range_mode,
            failing_statuses: Mutex::new(failing_statuses.into()),
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

            if let Some(status) = route.failing_statuses.lock().unwrap().pop_front() {
                return MockResponse {
                    status,
                    content_length: Some(0),
                    body: Vec::new(),
                };
            }

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
mod tests;

#[cfg(test)]
mod transport_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinSet};

    use super::*;

    #[derive(Clone, Copy)]
    enum Reply {
        Status(StatusCode),
        TruncatedBody,
    }

    struct Server {
        url: String,
        http1_requests: Arc<AtomicUsize>,
        http2_requests: Arc<AtomicUsize>,
        resumed_requests: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Server {
        async fn start(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/image", listener.local_addr().unwrap());
            let http1_requests = Arc::new(AtomicUsize::new(0));
            let http2_requests = Arc::new(AtomicUsize::new(0));
            let resumed_requests = Arc::new(AtomicUsize::new(0));
            let resumed_count = resumed_requests.clone();
            let http1_count = http1_requests.clone();
            let http2_count = http2_requests.clone();
            let task = tokio::spawn(async move {
                let mut connections = JoinSet::new();
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let http1_count = http1_count.clone();
                    let http2_count = http2_count.clone();
                    let replies = replies.clone();
                    let resumed_count = resumed_count.clone();
                    connections.spawn(async move {
                        let mut first = [0; 1];
                        stream.peek(&mut first).await.unwrap();
                        if first[0] == b'P' {
                            let mut connection = h2::server::handshake(stream).await.unwrap();
                            while let Some(Ok((_, mut response))) = connection.accept().await {
                                http2_count.fetch_add(1, Ordering::SeqCst);
                                response.send_reset(h2::Reason::REFUSED_STREAM);
                            }
                            return;
                        }

                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            request.push(stream.read_u8().await.unwrap());
                        }
                        let index = http1_count.fetch_add(1, Ordering::SeqCst);
                        let reply = replies
                            .get(index)
                            .copied()
                            .unwrap_or(Reply::Status(StatusCode::OK));
                        let status = match reply {
                            Reply::Status(status) => status,
                            Reply::TruncatedBody => StatusCode::OK,
                        };
                        let request = String::from_utf8(request).unwrap();
                        let (status, body, range) = if status == StatusCode::OK
                            && request.to_ascii_lowercase().contains("range: bytes=3-\r\n")
                        {
                            resumed_count.fetch_add(1, Ordering::SeqCst);
                            (
                                StatusCode::PARTIAL_CONTENT,
                                "def",
                                "Content-Range: bytes 3-5/6\r\n",
                            )
                        } else {
                            (status, "abcdef", "")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{range}Connection: \
                             close\r\n\r\n{body}",
                            body.len() + usize::from(matches!(reply, Reply::TruncatedBody))
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            });
            Self {
                url,
                http1_requests,
                http2_requests,
                resumed_requests,
                task,
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn registry_requests_recover_transient_errors_and_bound_failures() {
        for (replies, expected_requests, expected_error) in [
            (
                vec![
                    Reply::Status(StatusCode::TOO_MANY_REQUESTS),
                    Reply::Status(StatusCode::GATEWAY_TIMEOUT),
                ],
                3,
                None,
            ),
            (vec![Reply::TruncatedBody], 2, None),
            (
                vec![Reply::Status(StatusCode::NOT_FOUND)],
                1,
                Some(StatusCode::NOT_FOUND),
            ),
            (
                vec![Reply::Status(StatusCode::SERVICE_UNAVAILABLE); 5],
                5,
                Some(StatusCode::SERVICE_UNAVAILABLE),
            ),
        ] {
            let server = Server::start(replies).await;
            let client = http_client().unwrap();
            let result =
                tokio::time::timeout(Duration::from_secs(5), fetch_text(&client, &server.url))
                    .await
                    .unwrap();
            if let Some(status) = expected_error {
                let error = result.unwrap_err();
                assert_eq!(
                    error.downcast_ref::<reqwest::Error>().unwrap().status(),
                    Some(status)
                );
            } else {
                assert_eq!(result.unwrap(), "abcdef");
            }
            assert_eq!(
                server.http1_requests.load(Ordering::SeqCst),
                expected_requests
            );
        }
    }

    async fn assert_http2_recovery(archive: bool) {
        let server = Server::start(Vec::new()).await;
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let operation = async {
            if archive {
                let workspace = tempfile::tempdir().unwrap();
                let path = workspace.path().join("archive");
                fs::write(part_path(&path), b"abc").unwrap();
                let digest = format!("{:x}", Sha256::digest(b"abcdef"));
                assert_eq!(
                    download_file_verified_sha256(&client, &server.url, &path, &digest)
                        .await
                        .unwrap(),
                    DownloadOutcome::Downloaded
                );
                assert_eq!(fs::read(&path).unwrap(), b"abcdef");
                assert!(!part_path(&path).exists());
            } else {
                assert_eq!(fetch_text(&client, &server.url).await.unwrap(), "abcdef");
            }
        };
        tokio::time::timeout(Duration::from_secs(10), operation)
            .await
            .unwrap();
        assert!(server.http2_requests.load(Ordering::SeqCst) > 0);
        assert_eq!(server.http1_requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.resumed_requests.load(Ordering::SeqCst),
            usize::from(archive)
        );
    }

    #[tokio::test]
    async fn archive_http2_recovery_preserves_partial_bytes() {
        assert_http2_recovery(true).await;
    }

    #[tokio::test]
    async fn registry_http2_recovery_uses_remaining_attempts() {
        assert_http2_recovery(false).await;
        let server = Server::start(vec![Reply::Status(StatusCode::SERVICE_UNAVAILABLE); 5]).await;
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(10), fetch_text(&client, &server.url))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        assert!(server.http2_requests.load(Ordering::SeqCst) > 0);
        assert_eq!(server.http1_requests.load(Ordering::SeqCst), 4);
    }
}

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

#[tokio::test]
async fn download_file_retries_transient_http_status() {
    let server =
        TestServer::start_with_failures(b"abcdef".to_vec(), vec![StatusCode::GATEWAY_TIMEOUT])
            .await;
    let workspace = tempdir().unwrap();
    let output_path = workspace.path().join("rootfs.img.tar.gz");

    let client = http_client().unwrap();
    download_file(&client, &server.url(), &output_path)
        .await
        .unwrap();

    assert_eq!(fs::read(&output_path).unwrap(), b"abcdef");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn download_file_does_not_retry_permanent_http_status() {
    let server =
        TestServer::start_with_failures(b"abcdef".to_vec(), vec![StatusCode::NOT_FOUND]).await;
    let workspace = tempdir().unwrap();
    let output_path = workspace.path().join("rootfs.img.tar.gz");

    let client = http_client().unwrap();
    let err = download_file(&client, &server.url(), &output_path)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("HTTP 404 Not Found"));
    assert_eq!(server.request_count(), 1);
}

struct TestServer {
    handle: test_support::MockHandle,
}

impl TestServer {
    async fn start_with_failures(body: Vec<u8>, statuses: Vec<StatusCode>) -> Self {
        Self {
            handle: test_support::register_download_with_failures(
                "rootfs.img.tar.gz",
                body,
                test_support::MockRangeMode::Ignore,
                statuses,
            ),
        }
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

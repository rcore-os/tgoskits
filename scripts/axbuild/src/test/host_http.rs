use std::{
    fs,
    io::{ErrorKind, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use crate::test::case::HostHttpServerConfig;

const BIND_RETRY_TIMEOUT: Duration = Duration::from_secs(180);
const BIND_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Per-write timeout while streaming the body. A large multi-megabyte payload
/// can apply TCP backpressure for several seconds whenever a slow guest stalls
/// draining its receive window (notably x86_64, which is the slowest QEMU
/// guest). The timeout must therefore be generous enough not to abort a
/// legitimately progressing transfer, yet finite so a wedged guest cannot block
/// the server thread (and thus `Drop`/`join`) forever.
const BODY_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_STALL_TIMEOUT: Duration = Duration::from_secs(180);
const BODY_CHUNK_SIZE: usize = 16 * 1024;

pub(crate) struct HostHttpServerGuard {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostHttpServerGuard {
    pub(crate) fn start(config: &HostHttpServerConfig, case_name: &str) -> anyhow::Result<Self> {
        let addr = format!("{}:{}", config.bind, config.port);
        let listener = bind_listener(&addr, case_name)?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("failed to configure host HTTP server `{addr}`"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let body = HostHttpBody::from_config(config)
            .with_context(|| format!("invalid host HTTP server config for `{case_name}`"))?;
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = ready_tx.send(());
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                        let _ = stream.set_write_timeout(Some(BODY_WRITE_TIMEOUT));
                        let mut request = [0; 2048];
                        let n = stream.read(&mut request).unwrap_or(0);
                        body.respond(&mut stream, &request[..n]);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        if ready_rx.recv_timeout(Duration::from_secs(1)).is_err() {
            stop.store(true, Ordering::Release);
            bail!("host HTTP server `{addr}` did not become ready for `{case_name}`");
        }

        println!("  host http server: {addr}");
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

fn bind_listener(addr: &str, case_name: &str) -> anyhow::Result<TcpListener> {
    let started = Instant::now();
    let mut reported_wait = false;

    loop {
        match TcpListener::bind(addr) {
            Ok(listener) => return Ok(listener),
            Err(err)
                if err.kind() == std::io::ErrorKind::AddrInUse
                    && started.elapsed() < BIND_RETRY_TIMEOUT =>
            {
                if !reported_wait {
                    println!(
                        "  host http server: waiting for {addr} to become available for \
                         {case_name}"
                    );
                    reported_wait = true;
                }
                thread::sleep(BIND_RETRY_INTERVAL);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to bind host HTTP server `{addr}` for `{case_name}` after waiting \
                         up to {BIND_RETRY_TIMEOUT:?}"
                    )
                });
            }
        }
    }
}

#[derive(Debug, Clone)]
enum HostHttpBody {
    Static(Vec<u8>),
    Generated {
        size: usize,
        byte: u8,
    },
    /// Path-routed static file server: `/` returns an autoindex of the directory
    /// and `/<file>` returns that file's bytes (404 otherwise). Used to serve a
    /// local wheel index for hermetic online `pip/uv install --find-links`.
    Dir(PathBuf),
}

impl HostHttpBody {
    fn from_config(config: &HostHttpServerConfig) -> anyhow::Result<Self> {
        if let Some(dir) = &config.dir {
            let dir = resolve_serve_dir(dir)?;
            return Ok(Self::Dir(dir));
        }
        Ok(match config.body_size {
            Some(size) => Self::Generated {
                size,
                byte: config.body_byte,
            },
            None => Self::Static(config.body.as_bytes().to_vec()),
        })
    }

    fn len(&self) -> usize {
        match self {
            Self::Static(body) => body.len(),
            Self::Generated { size, .. } => *size,
            Self::Dir(_) => 0,
        }
    }

    /// Write a complete HTTP/1.1 response for one request to `stream`.
    fn respond(&self, stream: &mut impl Write, request: &[u8]) {
        match self {
            Self::Dir(dir) => serve_from_dir(stream, request, dir),
            _ => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    self.len(),
                );
                if stream.write_all(head.as_bytes()).is_ok() {
                    // Surface a truncated send instead of silently closing the
                    // connection mid-body, which the client would only see as an
                    // opaque short read.
                    if let Err(err) = self.write_to(stream) {
                        eprintln!("  host http server: body write failed: {err}");
                    }
                }
            }
        }
    }

    fn write_to(&self, stream: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Static(body) => write_body_chunks(stream, body.len(), |offset, len| {
                &body[offset..offset + len]
            }),
            Self::Generated { size, byte } => {
                let chunk = vec![*byte; BODY_CHUNK_SIZE];
                write_body_chunks(stream, *size, |_offset, len| &chunk[..len])
            }
            Self::Dir(_) => Ok(()),
        }
    }
}

/// Resolve a `[host_http_server] dir` value to a deterministic absolute path and
/// fail fast if it does not exist or cannot be read.
///
/// `dir` is interpreted as workspace-root-relative (matching the `qemu-*.toml`
/// comment) when given as a relative path, so the served directory does not
/// depend on the process CWD. Absolute paths are used verbatim. A missing or
/// unreadable directory is a hard error here rather than a silent empty `200`
/// index that would leak into the guest as a confusing install failure.
fn resolve_serve_dir(dir: &str) -> anyhow::Result<PathBuf> {
    let raw = Path::new(dir);
    let resolved = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        crate::context::workspace_root_path()
            .context("failed to resolve workspace root for host HTTP server `dir`")?
            .join(raw)
    };

    let canonical = resolved.canonicalize().with_context(|| {
        format!(
            "host HTTP server `dir` does not exist or is not accessible: {}",
            resolved.display()
        )
    })?;
    if !canonical.is_dir() {
        bail!(
            "host HTTP server `dir` is not a directory: {}",
            canonical.display()
        );
    }
    // Confirm the directory is actually readable now, so a hard error surfaces at
    // startup instead of an empty autoindex once the guest starts requesting it.
    fs::read_dir(&canonical).with_context(|| {
        format!(
            "host HTTP server `dir` is not readable: {}",
            canonical.display()
        )
    })?;
    Ok(canonical)
}

/// Parse the request line, then serve an autoindex (for `/`) or a single file.
/// Only flat filenames are served; any `/` or `..` in the name yields 404, so a
/// request cannot escape `dir`.
fn serve_from_dir(stream: &mut impl Write, request: &[u8], dir: &Path) {
    let head = String::from_utf8_lossy(request);
    let raw_path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");

    if path == "/" || path.is_empty() {
        let mut names: Vec<String> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        let mut html = String::from("<!DOCTYPE html><html><body>\n");
        for name in &names {
            html.push_str(&format!("<a href=\"{name}\">{name}</a>\n"));
        }
        html.push_str("</body></html>\n");
        write_dir_response(stream, "200 OK", "text/html", html.as_bytes());
        return;
    }

    let name = path.trim_start_matches('/');
    if name.is_empty() || name.contains('/') || name.contains("..") {
        write_dir_response(stream, "404 Not Found", "text/plain", b"not found\n");
        return;
    }
    match fs::read(dir.join(name)) {
        Ok(bytes) => write_dir_response(stream, "200 OK", "application/octet-stream", &bytes),
        Err(_) => write_dir_response(stream, "404 Not Found", "text/plain", b"not found\n"),
    }
}

fn write_dir_response(stream: &mut impl Write, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n",
        body.len(),
    );
    if stream.write_all(head.as_bytes()).is_ok() {
        let _ = write_body_chunks(stream, body.len(), |offset, len| &body[offset..offset + len]);
    }
}

fn write_body_chunks<'a>(
    stream: &mut impl Write,
    total: usize,
    mut chunk_at: impl FnMut(usize, usize) -> &'a [u8],
) -> std::io::Result<()> {
    let mut written = 0;
    let mut last_progress = Instant::now();

    while written < total {
        let len = (total - written).min(BODY_CHUNK_SIZE);
        let chunk = chunk_at(written, len);
        let mut chunk_written = 0;

        while chunk_written < len {
            match stream.write(&chunk[chunk_written..]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::WriteZero,
                        "host HTTP body stream stopped accepting bytes",
                    ));
                }
                Ok(n) => {
                    chunk_written += n;
                    written += n;
                    last_progress = Instant::now();
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    if last_progress.elapsed() >= BODY_STALL_TIMEOUT {
                        return Err(err);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(err),
            }
        }
    }

    Ok(())
}

impl Drop for HostHttpServerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::*;

    #[test]
    fn generated_body_serves_configured_size_and_byte() {
        let port = free_local_port();
        let config = HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port,
            body: "unused".to_string(),
            body_size: Some(7),
            body_byte: b'X',
            dir: None,
        };

        let _guard = HostHttpServerGuard::start(&config, "generated-body").unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /payload.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();

        let body_offset = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .unwrap();
        let headers = String::from_utf8_lossy(&response[..body_offset]);
        assert!(headers.contains("Content-Length: 7"));
        assert_eq!(&response[body_offset..], b"XXXXXXX");
    }

    #[test]
    #[cfg(unix)]
    fn same_host_http_port_waits_for_active_guard() {
        let port = free_local_port();
        let config = HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port,
            body: "first".to_string(),
            body_size: None,
            body_byte: b'A',
            dir: None,
        };

        let first_guard = HostHttpServerGuard::start(&config, "first").unwrap();
        let second_config = config.clone();
        let (tx, rx) = mpsc::channel();
        let second_thread = thread::spawn(move || {
            let result = HostHttpServerGuard::start(&second_config, "second").map(|_guard| ());
            tx.send(result.map_err(|err| err.to_string())).unwrap();
        });

        match rx.recv_timeout(Duration::from_millis(100)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(err) => panic!("second host HTTP guard channel failed: {err}"),
            Ok(result) => {
                panic!("second host HTTP guard did not wait for the first guard: {result:?}")
            }
        }

        drop(first_guard);

        let result = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second host HTTP guard did not start after the first guard dropped");
        assert!(result.is_ok(), "{result:?}");
        second_thread.join().unwrap();
    }

    #[test]
    fn relative_dir_resolves_against_workspace_root() {
        // The pip-uv online wheel index is a committed, workspace-root-relative
        // directory; resolving it must yield an absolute path under the workspace
        // root regardless of the test process CWD.
        let rel = "apps/starry/pip-uv/online-index";
        let resolved = resolve_serve_dir(rel).expect("relative dir should resolve");
        let workspace_root =
            crate::context::workspace_root_path().expect("workspace root should resolve");

        assert!(resolved.is_absolute(), "resolved dir must be absolute");
        assert!(resolved.starts_with(&workspace_root));
        assert_eq!(resolved, workspace_root.join(rel).canonicalize().unwrap());

        match HostHttpBody::from_config(&HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
            body: "unused".to_string(),
            body_size: None,
            body_byte: b'a',
            dir: Some(rel.to_string()),
        })
        .expect("from_config should accept the committed relative dir")
        {
            HostHttpBody::Dir(path) => assert_eq!(path, resolved),
            other => panic!("expected Dir body, got {other:?}"),
        }
    }

    #[test]
    fn absolute_dir_is_used_verbatim() {
        let temp = std::env::temp_dir();
        let resolved = resolve_serve_dir(&temp.to_string_lossy())
            .expect("an existing absolute dir should resolve");
        assert_eq!(resolved, temp.canonicalize().unwrap());
    }

    #[test]
    fn missing_dir_errors_at_start() {
        let config = HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: free_local_port(),
            body: "unused".to_string(),
            body_size: None,
            body_byte: b'a',
            // A path that cannot exist under the workspace root.
            dir: Some("apps/starry/pip-uv/__definitely_missing_index__".to_string()),
        };

        let err = match HostHttpServerGuard::start(&config, "missing-dir") {
            Ok(_) => {
                panic!("a missing host HTTP dir must be a hard error, not a silent empty index")
            }
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(
            message.contains("does not exist or is not accessible"),
            "unexpected error message: {message}"
        );
    }

    fn free_local_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
}

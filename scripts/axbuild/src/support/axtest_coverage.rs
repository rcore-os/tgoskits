use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use ostool::{build::config::Cargo, run::qemu::QemuConfig};

pub(crate) const AXTEST_COVERAGE_RUSTFLAGS: &[&str] = &[
    "--cfg",
    "axtest_coverage",
    "--check-cfg",
    "cfg(axtest_coverage)",
    "-Cinstrument-coverage",
    "-Zno-profiler-runtime",
];

const COVERAGE_FEATURE: &str = "axtest/coverage";
const MARKER_PREFIX: &str = "AXTEST_COVERAGE status=ready";
const SUITE_OK_MARKER: &str = "AXTEST_SUITE_OK";
pub(crate) const COVERAGE_DONE_MARKER: &str = "AXTEST_COVERAGE_DONE";

pub(crate) fn enabled(cargo: &Cargo) -> bool {
    crate::build::env_truthy(&cargo.env, "AXTEST_COVERAGE")
}

pub(crate) fn prepare_cargo(cargo: &mut Cargo) {
    if !cargo
        .features
        .iter()
        .any(|feature| feature == COVERAGE_FEATURE)
    {
        cargo.features.push(COVERAGE_FEATURE.to_string());
    }
    crate::build::append_encoded_rustflags(cargo, AXTEST_COVERAGE_RUSTFLAGS);
}

#[derive(Debug, Clone)]
pub(crate) struct AxtestCoveragePaths {
    pub(crate) monitor_socket: PathBuf,
    pub(crate) profraw_path: PathBuf,
}

impl AxtestCoveragePaths {
    pub(crate) fn new(workspace_root: &Path, package: &str, target: &str) -> anyhow::Result<Self> {
        let arch_triple = Path::new(target)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| sanitize_path_component(target));
        let profraw_filename = format!("{package}-{arch_triple}.profraw");
        let dir = workspace_root.join("coverage");
        fs::create_dir_all(&dir)?;
        let profraw_path = dir.join(profraw_filename);
        let mut hasher = DefaultHasher::new();
        profraw_path.hash(&mut hasher);
        let socket_name = format!("axcov-{}-{:016x}.sock", std::process::id(), hasher.finish());
        Ok(Self {
            monitor_socket: std::env::temp_dir().join(socket_name),
            profraw_path,
        })
    }
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn apply_qemu_monitor(qemu: &mut QemuConfig, paths: &AxtestCoveragePaths) {
    let _ = fs::remove_file(&paths.monitor_socket);
    let monitor = format!("unix:{},server,nowait", paths.monitor_socket.display());
    qemu.args.extend([
        "-monitor".to_string(),
        monitor,
        "-D".to_string(),
        paths
            .profraw_path
            .with_file_name("qemu.log")
            .display()
            .to_string(),
    ]);
}

/// Replace the QEMU success regex so that ostool waits for coverage extraction
/// to complete (signaled by `AXTEST_COVERAGE_DONE`) instead of matching
/// `AXTEST_SUITE_OK` prematurely.
pub(crate) fn update_success_regex(qemu: &mut QemuConfig) {
    for regex in &mut qemu.success_regex {
        if regex.contains(SUITE_OK_MARKER) {
            *regex = regex.replace(SUITE_OK_MARKER, COVERAGE_DONE_MARKER);
        }
    }
    // If no success regex contained the marker, add one for coverage done.
    if !qemu
        .success_regex
        .iter()
        .any(|r| r.contains(COVERAGE_DONE_MARKER))
    {
        qemu.success_regex.push(COVERAGE_DONE_MARKER.to_string());
    }
}

#[cfg(unix)]
mod capture {
    use std::{
        fs,
        io::{self, Read, Write},
        os::{fd::FromRawFd, unix::net::UnixStream},
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail};
    use regex::Regex;

    use super::{AxtestCoveragePaths, COVERAGE_DONE_MARKER, MARKER_PREFIX, SUITE_OK_MARKER};

    pub(crate) struct AxtestCoverageCaptureGuard {
        saved_stdout: i32,
        saved_stderr: i32,
        reader: Option<JoinHandle<io::Result<()>>>,
        state: Arc<Mutex<AxtestCoverageState>>,
    }

    #[derive(Debug)]
    struct AxtestCoverageState {
        monitor_socket: PathBuf,
        profraw_path: PathBuf,
        line_buf: String,
        dumped: bool,
        completion_signaled: bool,
        error: Option<String>,
        monitor_conn: Option<UnixStream>,
    }

    impl AxtestCoverageCaptureGuard {
        pub(crate) fn install(paths: &AxtestCoveragePaths) -> io::Result<Self> {
            let saved_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
            let saved_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
            if saved_stdout < 0 || saved_stderr < 0 {
                return Err(io::Error::last_os_error());
            }

            let tee_stdout = unsafe { libc::dup(saved_stdout) };
            if tee_stdout < 0 {
                return Err(io::Error::last_os_error());
            }

            let mut fds = [0i32; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let read_fd = fds[0];
            let write_fd = fds[1];
            if unsafe { libc::dup2(write_fd, libc::STDOUT_FILENO) } < 0
                || unsafe { libc::dup2(write_fd, libc::STDERR_FILENO) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            unsafe { libc::close(write_fd) };

            let state = Arc::new(Mutex::new(AxtestCoverageState {
                monitor_socket: paths.monitor_socket.clone(),
                profraw_path: paths.profraw_path.clone(),
                line_buf: String::new(),
                dumped: false,
                completion_signaled: false,
                error: None,
                monitor_conn: None,
            }));

            // Pre-connect to the QEMU monitor socket in a background thread.
            // This avoids a race condition where QEMU exits (after matching the
            // success pattern) before the reader thread can connect to the socket.
            let connector_state = state.clone();
            let socket_path = paths.monitor_socket.clone();
            std::thread::spawn(move || {
                if let Ok(conn) = wait_and_connect_monitor(&socket_path)
                    && let Ok(mut state) = connector_state.lock()
                {
                    state.monitor_conn = Some(conn);
                }
            });

            let reader_state = state.clone();
            let reader = std::thread::spawn(move || {
                let mut pipe = unsafe { fs::File::from_raw_fd(read_fd) };
                let mut terminal = unsafe { fs::File::from_raw_fd(tee_stdout) };
                let mut tee_buf = String::new();
                let mut buf = [0u8; 8192];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            tee_buf.push_str(&chunk);
                            // Flush complete lines to terminal, filtering out
                            // AXTEST_SUITE_OK so ostool doesn't kill QEMU before
                            // coverage extraction finishes.
                            while let Some(newline) = tee_buf.find('\n') {
                                let line = &tee_buf[..=newline];
                                if !line.contains(SUITE_OK_MARKER) {
                                    terminal.write_all(line.as_bytes())?;
                                }
                                tee_buf.drain(..=newline);
                            }
                            if let Ok(mut state) = reader_state.lock() {
                                state.push_bytes(&buf[..n]);
                                // If coverage was just extracted, signal completion
                                // to ostool so it can stop waiting.
                                if state.dumped && !state.completion_signaled {
                                    state.completion_signaled = true;
                                    let marker = format!("{COVERAGE_DONE_MARKER}\n");
                                    terminal.write_all(marker.as_bytes())?;
                                }
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        Err(err) => return Err(err),
                    }
                }
                // Flush any remaining partial line
                if !tee_buf.is_empty() && !tee_buf.contains(SUITE_OK_MARKER) {
                    terminal.write_all(tee_buf.as_bytes())?;
                }
                terminal.flush()
            });

            Ok(Self {
                saved_stdout,
                saved_stderr,
                reader: Some(reader),
                state,
            })
        }

        pub(crate) fn finish(mut self) -> anyhow::Result<()> {
            self.restore();
            if let Some(reader) = self.reader.take() {
                reader.join().map_err(|payload| {
                    let msg = payload
                        .downcast_ref::<&'static str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().map(|s| s.clone()))
                        .unwrap_or_else(|| "<non-string panic payload>".to_string());
                    anyhow::anyhow!("axtest coverage capture thread panicked: {msg}")
                })??;
            }
            let state = self.state.lock().unwrap();
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            if state.dumped {
                println!("  coverage: {}", state.profraw_path.display());
            } else {
                bail!(
                    "axtest coverage was enabled but no coverage profile was captured at {}",
                    state.profraw_path.display()
                );
            }
            Ok(())
        }

        fn restore(&self) {
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            unsafe {
                libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
                libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
            }
        }
    }

    impl Drop for AxtestCoverageCaptureGuard {
        fn drop(&mut self) {
            self.restore();
            unsafe {
                libc::close(self.saved_stdout);
                libc::close(self.saved_stderr);
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    impl AxtestCoverageState {
        fn push_bytes(&mut self, bytes: &[u8]) {
            self.line_buf.push_str(&String::from_utf8_lossy(bytes));
            while let Some(newline) = self.line_buf.find('\n') {
                let line = self.line_buf[..newline].trim_end_matches('\r').to_string();
                self.line_buf.drain(..=newline);
                self.process_line(&line);
            }
        }

        fn process_line(&mut self, line: &str) {
            if self.dumped || !line.starts_with(MARKER_PREFIX) {
                return;
            }
            match parse_coverage_marker(line).and_then(|(addr, size)| {
                self.dump_coverage(addr, size)
                    .map_err(|err| err.to_string())
            }) {
                Ok(()) => self.dumped = true,
                Err(err) => self.error = Some(err),
            }
        }

        fn dump_coverage(&mut self, addr: u64, size: usize) -> anyhow::Result<()> {
            let mut stream = self
                .monitor_conn
                .take()
                .or_else(|| {
                    // Fallback: connect on demand if pre-connection wasn't ready.
                    wait_and_connect_monitor(&self.monitor_socket).ok()
                })
                .with_context(|| {
                    format!(
                        "QEMU monitor socket was not available at {}",
                        self.monitor_socket.display()
                    )
                })?;
            let command = format!(
                "memsave 0x{addr:x} {size} \"{}\"\n",
                self.profraw_path.display()
            );
            stream
                .write_all(command.as_bytes())
                .context("failed to send QEMU memsave command")?;
            stream.flush().ok();
            Ok(())
        }
    }

    fn wait_and_connect_monitor(socket: &Path) -> anyhow::Result<UnixStream> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if socket.exists() {
                return UnixStream::connect(socket).with_context(|| {
                    format!("failed to connect QEMU monitor at {}", socket.display())
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!(
            "QEMU monitor socket was not created at {}",
            socket.display()
        )
    }

    fn parse_coverage_marker(line: &str) -> Result<(u64, usize), String> {
        let regex = Regex::new(r"\baddr=0x([0-9a-fA-F]+)\s+size=([0-9]+)\b").unwrap();
        let caps = regex
            .captures(line)
            .ok_or_else(|| format!("invalid axtest coverage marker: {line}"))?;
        let addr = u64::from_str_radix(&caps[1], 16)
            .map_err(|err| format!("invalid coverage address in `{line}`: {err}"))?;
        let size = caps[2]
            .parse::<usize>()
            .map_err(|err| format!("invalid coverage size in `{line}`: {err}"))?;
        if size == 0 {
            return Err("coverage profile size is zero".to_string());
        }
        Ok((addr, size))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_marker_extracts_address_and_size() {
            assert_eq!(
                parse_coverage_marker("AXTEST_COVERAGE status=ready addr=0x1234abcd size=4096"),
                Ok((0x1234abcd, 4096))
            );
        }
    }
}

#[cfg(not(unix))]
mod capture {
    use super::AxtestCoveragePaths;

    pub(crate) struct AxtestCoverageCaptureGuard;

    impl AxtestCoverageCaptureGuard {
        pub(crate) fn install(_paths: &AxtestCoveragePaths) -> std::io::Result<Self> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "axtest coverage capture requires Unix QEMU monitor sockets",
            ))
        }

        pub(crate) fn finish(self) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

pub(crate) use capture::AxtestCoverageCaptureGuard;

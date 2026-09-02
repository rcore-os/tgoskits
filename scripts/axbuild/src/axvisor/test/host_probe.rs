//! Host-side probe runner for QEMU hostfwd integration tests.
//!
//! The probe is the reverse of the generic host fixture server
//! ([`crate::test::host_http`]): instead of serving host fixtures to the guest,
//! it acts as a *client* that dials the AxVisor management HTTP API running
//! *inside* the guest through QEMU user-mode networking
//! (`-netdev user,hostfwd=tcp::<host_port>-:<guest_port>`). The concrete HTTP
//! requests and assertions live with the test-suit case as an executable probe
//! asset (see [`crate::axvisor::test::http_probe`], which executes the asset
//! and collects its exit code); this module only provides the orchestration:
//! wait for the forwarded port, invoke the probe, and store its result as the
//! verdict. Nothing in the hypervisor knows a test is running.
//!
//! When the probe finishes — pass or fail — the guard quits QEMU over the QMP
//! monitor socket the runner added (`-qmp unix:...,server=on,wait=off`), so the
//! QEMU process exits cleanly and the runner reads the stored verdict from the
//! guard as the test result. The runner owns the QEMU child, so the case
//! `timeout` remains the backstop if the probe or its QMP quit fails.

use std::{
    io::Write,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use super::types::AxvisorHttpProbeConfig;

/// Captured probe output and its pass/fail verdict.
pub(crate) struct HostHttpProbeOutcome {
    pub(crate) output: Vec<u8>,
    pub(crate) verdict: anyhow::Result<()>,
}

impl HostHttpProbeOutcome {
    pub(crate) fn failed(error: anyhow::Error) -> Self {
        Self {
            output: Vec::new(),
            verdict: Err(error),
        }
    }

    fn append_diagnostic(&mut self, args: core::fmt::Arguments<'_>) {
        if !self.output.is_empty() && !self.output.ends_with(b"\n") {
            self.output.push(b'\n');
        }
        let _ = writeln!(&mut self.output, "{args}");
    }
}

/// The probe callback invoked by the guard once the forwarded port accepts.
/// The probe is a `FnOnce` so it may own everything it needs and runs on the
/// guard's worker thread without writing to QEMU's terminal stream.
pub(crate) type HostHttpProbeFn = Box<dyn FnOnce() -> HostHttpProbeOutcome + Send + 'static>;

/// Sleep between readiness retries.
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// How long to keep retrying the QMP connect before giving up on quitting QEMU.
const QMP_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const QMP_CONNECT_RETRIES: usize = 10;
/// How long to keep a QMP `quit` connection open waiting for QEMU to exit
/// before re-issuing `quit` on a fresh connection.
const QMP_EXIT_WAIT: Duration = Duration::from_secs(4);
/// Poll interval while waiting for QEMU to exit after a `quit`.
const QMP_READ_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How many times to re-issue `quit` before giving up and letting the case
/// timeout fail the run. QEMU drops a `quit` that arrives while the guest is
/// still tearing a VM down, so a stuck QEMU must be re-quit rather than left to
/// time out.
const QMP_QUIT_RETRIES: usize = 4;

pub(crate) struct HostHttpProbeGuard {
    stop: Arc<AtomicBool>,
    result: Arc<Mutex<Option<HostHttpProbeOutcome>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostHttpProbeGuard {
    /// Spawn the probe runner thread and return a guard that owns its
    /// lifecycle.
    ///
    /// `probe` is the host-side probe callback (the typed HTTP assertions);
    /// the guard waits for the forwarded port to accept connections, invokes
    /// it, and stores its result as the verdict. `qmp_socket` is the path QEMU
    /// binds from its `-qmp unix:...` argument; the guard connects to it after
    /// the probe finishes to quit QEMU. When `None`, the guard only stores the
    /// verdict and relies on the case timeout to end the run.
    ///
    /// `stop` is the shared abort flag the probe's poll loops check so a run
    /// whose QEMU already failed (fail_regex match, timeout, spawn error) can
    /// abort the probe thread on its next poll instead of waiting out the
    /// deadline. The runner owns it: it stores `true` when the case is over.
    pub(crate) fn start(
        config: &AxvisorHttpProbeConfig,
        host_port: u16,
        case_name: &str,
        qmp_socket: Option<PathBuf>,
        stop: Arc<AtomicBool>,
        probe: HostHttpProbeFn,
    ) -> anyhow::Result<Self> {
        let addr = format!("127.0.0.1:{host_port}");
        let connect_timeout = Duration::from_secs(config.connect_timeout_secs);
        let thread_stop = stop.clone();
        let result = Arc::new(Mutex::new(None));
        let thread_result = result.clone();
        let case_name = case_name.to_string();
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread_addr = addr.clone();
        let thread_case_name = case_name.clone();
        let thread = thread::spawn(move || {
            let _ = ready_tx.send(());
            // The guard waits for the forwarded port (guest boot + network
            // init); the probe then runs the HTTP assertions. The probe is
            // consumed exactly once.
            let mut outcome = (|| -> anyhow::Result<HostHttpProbeOutcome> {
                wait_for_port_ready(&thread_addr, connect_timeout, &thread_stop).with_context(
                    || {
                        format!(
                            "guest HTTP server never became reachable within {connect_timeout:?}"
                        )
                    },
                )?;
                Ok(probe())
            })()
            .unwrap_or_else(HostHttpProbeOutcome::failed);
            // Quit QEMU so a successful run ends promptly on the probe verdict
            // instead of the serial-timeout path. The runner owns the QEMU
            // child, so it decides whether QEMU actually exits: a `quit` that
            // is ignored degrades to the case timeout, which then fails the
            // run — a stuck QEMU must not be reported as a probe success.
            if let Some(socket) = qmp_socket
                && let Err(err) = request_qmp_quit(&socket)
            {
                outcome.append_diagnostic(format_args!(
                    "  host http probe: {thread_case_name}: failed to quit QEMU via QMP: {err:#}"
                ));
            }
            *thread_result.lock().unwrap() = Some(outcome);
        });

        if ready_rx.recv_timeout(Duration::from_secs(1)).is_err() {
            stop.store(true, Ordering::Release);
            bail!("host http probe for `{case_name}` did not become ready");
        }

        println!("  host http probe: {addr} -> guest:{}", config.guest_port);
        Ok(Self {
            stop,
            result,
            thread: Some(thread),
        })
    }

    /// Stop and join the worker, then take its captured output and verdict.
    ///
    /// On an early QEMU failure, setting `stop` makes a running probe terminate
    /// and publish its partial capture before this method reads the outcome.
    pub(crate) fn finish(mut self) -> Option<HostHttpProbeOutcome> {
        self.stop_and_join();
        self.result.lock().unwrap().take()
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HostHttpProbeGuard {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Poll the forwarded host port until a TCP connection succeeds, the deadline
/// elapses, or a stop is requested. A successful connect means the guest's
/// network stack is up; the in-guest server may still be booting, so the probe
/// itself should retry its first request.
fn wait_for_port_ready(
    addr: &str,
    connect_timeout: Duration,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            bail!("host http probe stopped");
        }
        if started.elapsed() >= connect_timeout {
            bail!("timed out after {connect_timeout:?}");
        }
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        thread::sleep(CONNECT_RETRY_INTERVAL);
    }
}

/// Quit QEMU by connecting to its QMP monitor socket and issuing `quit`. The
/// socket path comes from the `-qmp unix:...,server=on,wait=off` argument the
/// runner added. Returns once QEMU has begun exiting, or `Ok` after all retries
/// are exhausted (the runner's case timeout then fails the run — a stuck QEMU
/// must not be reported as a probe success).
///
/// Two QEMU quirks shape this routine:
///
/// - A `quit` that arrives while the guest is still tearing a VM down is
///   silently dropped (the QMP monitor stays responsive, but no shutdown
///   happens). The probe's final poll can see the VM removed from the HTTP layer
///   before the guest's `Dropping VM[..]` cleanup finishes, so the guard may
///   send `quit` into that window. To avoid every probe case hitting the case
///   timeout on this race, re-issue `quit` on a fresh connection if QEMU is
///   still alive after a wait window.
/// - QEMU only shuts down cleanly when the `quit` connection stays open: closing
///   it right after writing `quit` makes QEMU hang during exit while the guest
///   vCPU spins in a tight PL011 poll (TCG cannot preempt the translation block,
///   so the exit never completes). The guard therefore keeps the connection
///   alive and waits for the exit signal — EOF on the held stream, or the
///   listener socket refusing new connects.
#[cfg(unix)]
fn request_qmp_quit(socket: &Path) -> anyhow::Result<()> {
    use std::{
        io::{ErrorKind, Read, Write},
        os::unix::net::UnixStream,
    };

    /// Connect with retries, reporting the last error if QEMU never bound the
    /// socket.
    fn connect_with_retries(socket: &Path) -> anyhow::Result<UnixStream> {
        let mut last_err = None;
        for _ in 0..QMP_CONNECT_RETRIES {
            match UnixStream::connect(socket) {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    last_err = Some(err);
                    thread::sleep(QMP_CONNECT_RETRY_INTERVAL);
                }
            }
        }
        bail!(
            "failed to connect QMP socket {}: {}",
            socket.display(),
            last_err.as_ref().expect("at least one connect attempted")
        )
    }

    /// Do the QMP handshake (greeting, capabilities) and write `quit`, leaving
    /// the connection open.
    fn qmp_handshake_quit(stream: &mut UnixStream) -> std::io::Result<()> {
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .ok();
        let mut buf = [0_u8; 512];
        let _ = stream.read(&mut buf); // QMP greeting
        stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\r\n")?;
        buf.fill(0);
        let _ = stream.read(&mut buf); // capabilities response
        stream.write_all(b"{\"execute\":\"quit\"}\r\n")?;
        stream.flush()
    }

    /// Whether the socket path still accepts connections. A refused or missing
    /// socket means QEMU closed its listener (exiting or exited).
    fn socket_connectable(socket: &Path) -> bool {
        UnixStream::connect(socket).is_ok()
    }

    for _ in 0..QMP_QUIT_RETRIES {
        let mut stream = connect_with_retries(socket)?;
        if let Err(err) = qmp_handshake_quit(&mut stream) {
            // A failed handshake usually means QEMU was already closing its
            // listener (it began exiting from an earlier `quit`), so the run
            // can end. Surface the error only when QEMU is demonstrably still
            // listening.
            if socket_connectable(socket) {
                bail!("failed to send QMP quit: {err}");
            }
            return Ok(());
        }

        // Keep the connection open and wait for QEMU to exit. EOF on the held
        // stream, or the listener refusing connects, means QEMU is shutting
        // down; a still-connectable listener after the wait window means QEMU
        // dropped the `quit` (guest teardown in flight), so retry on a fresh
        // connection.
        let wait_started = Instant::now();
        loop {
            if wait_started.elapsed() >= QMP_EXIT_WAIT {
                break;
            }
            let mut buf = [0_u8; 512];
            match stream.read(&mut buf) {
                Ok(0) => return Ok(()), // EOF: QEMU closed the connection
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if !socket_connectable(socket) {
                        return Ok(()); // listener gone: QEMU exiting/exited
                    }
                }
                Err(_) => return Ok(()), // connection reset: QEMU gone
                Ok(_) => {}              // SHUTDOWN event / response; keep waiting
            }
            thread::sleep(QMP_READ_POLL_INTERVAL);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn request_qmp_quit(_socket: &Path) -> anyhow::Result<()> {
    bail!("QMP unix sockets are not supported on this host")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_joins_the_worker_before_taking_a_late_outcome() {
        let stop = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let thread_stop = stop.clone();
        let thread_result = result.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
            *thread_result.lock().unwrap() = Some(HostHttpProbeOutcome {
                output: b"partial probe output\n".to_vec(),
                verdict: Err(anyhow::anyhow!("probe stopped")),
            });
        });
        let guard = HostHttpProbeGuard {
            stop,
            result,
            thread: Some(thread),
        };

        let outcome = guard.finish().expect("late probe outcome");

        assert_eq!(outcome.output, b"partial probe output\n");
        assert!(outcome.verdict.is_err());
    }

    #[test]
    fn qmp_failure_diagnostic_is_buffered_with_probe_output() {
        let mut outcome = HostHttpProbeOutcome {
            output: b"probe output without newline".to_vec(),
            verdict: Ok(()),
        };

        outcome.append_diagnostic(format_args!("QMP quit failed: connection refused"));

        assert_eq!(
            outcome.output,
            b"probe output without newline\nQMP quit failed: connection refused\n"
        );
    }
}

use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::bail;
const QMP_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const QMP_CONNECT_RETRIES: usize = 10;
const QMP_EXIT_WAIT: Duration = Duration::from_secs(4);
const QMP_READ_POLL_INTERVAL: Duration = Duration::from_millis(100);
const QMP_QUIT_RETRIES: usize = 4;

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
pub(crate) fn request_qmp_quit(socket: &Path) -> anyhow::Result<()> {
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
pub(crate) fn request_qmp_quit(_socket: &Path) -> anyhow::Result<()> {
    bail!("QMP unix sockets are not supported on this host")
}

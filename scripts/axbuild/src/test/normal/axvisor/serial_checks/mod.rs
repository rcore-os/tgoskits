//! Ordered checks over a QEMU serial socket while HTTP checks share its lifetime.

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use ostool::run::ShellCheckStep;

#[cfg(unix)]
mod failures;

#[cfg(unix)]
mod unix {
    use std::{
        io::{self, Read, Write},
        os::unix::net::UnixStream,
        sync::{
            Condvar, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::Instant,
    };

    use anyhow::{Context, bail, ensure};
    use regex::Regex;

    use super::{failures::FailurePatterns, *};

    const POLL: Duration = Duration::from_millis(50);
    const MAX_PENDING: usize = 16 * 1024 * 1024;

    #[derive(Default)]
    struct State {
        pending: String,
        failure: Option<String>,
        closed: bool,
    }

    struct Shared {
        state: Mutex<State>,
        changed: Condvar,
        writer: Mutex<Option<UnixStream>>,
        cancelled: Arc<AtomicBool>,
    }

    impl Shared {
        fn fail(&self, message: String) {
            let mut state = self.state.lock().unwrap();
            state.failure.get_or_insert(message);
            self.cancelled.store(true, Ordering::Release);
            self.changed.notify_all();
        }

        fn verdict(&self) -> Result<()> {
            if let Some(message) = &self.state.lock().unwrap().failure {
                bail!("{message}");
            }
            Ok(())
        }
    }

    pub(crate) struct Monitor {
        shared: Arc<Shared>,
        stop: Arc<AtomicBool>,
        reader: Option<JoinHandle<()>>,
    }

    pub(crate) struct Checks {
        shared: Arc<Shared>,
        prefix: Option<String>,
        require_newline: bool,
    }

    impl Monitor {
        pub(crate) fn start(
            socket: &Path,
            failures: &[String],
            cancelled: Arc<AtomicBool>,
            timeout: Duration,
            mut output: impl Write + Send + 'static,
        ) -> Result<(Self, Checks)> {
            // Keep the same Unicode-aware regex semantics as ostool. A dense
            // streaming DFA cannot represent Unicode word boundaries.
            let matcher = FailurePatterns::new(failures)?;
            let shared = Arc::new(Shared {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
                writer: Mutex::new(None),
                cancelled,
            });
            let stop = Arc::new(AtomicBool::new(false));
            let worker = shared.clone();
            let worker_stop = stop.clone();
            let socket = socket.to_path_buf();
            let reader = thread::spawn(move || {
                let result = (|| -> Result<()> {
                    let deadline = Instant::now()
                        .checked_add(timeout)
                        .context("serial deadline overflow")?;
                    let mut stream = loop {
                        if worker_stop.load(Ordering::Acquire)
                            || worker.cancelled.load(Ordering::Acquire)
                        {
                            return Ok(());
                        }
                        match UnixStream::connect(&socket) {
                            Ok(stream) => break stream,
                            Err(error) => {
                                ensure!(
                                    Instant::now() < deadline,
                                    "connect QEMU serial {}: {error}",
                                    socket.display()
                                );
                                thread::sleep(POLL);
                            }
                        }
                    };
                    stream.set_read_timeout(Some(POLL))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    *worker.writer.lock().unwrap() = Some(stream.try_clone()?);
                    worker.changed.notify_all();
                    let mut buffer = [0u8; 8192];
                    let mut escape = 0u8;
                    let mut transcript = Vec::new();
                    let mut line_start = 0;
                    let mut utf8 = Vec::new();
                    let mut consume = |bytes: &[u8], last: bool| -> Result<()> {
                        output.write_all(bytes)?;
                        for &byte in bytes {
                            match (escape, byte) {
                                (0, 0x1b) => escape = 1,
                                (0, b'\r') => {}
                                (0, _) => utf8.push(byte),
                                (1, b'[') => escape = 2,
                                (1, _) => {
                                    escape = 0;
                                    utf8.push(byte);
                                }
                                (2, 0x40..=0x7e) => escape = 0,
                                _ => {}
                            }
                        }
                        let decoded = decode_utf8(&mut utf8, last);
                        let mut state = worker.state.lock().unwrap();
                        state.pending.push_str(&decoded);
                        ensure!(
                            state.pending.len() <= MAX_PENDING,
                            "QEMU serial pending output exceeded {MAX_PENDING} bytes"
                        );
                        let previous_length = transcript.len();
                        transcript.extend_from_slice(decoded.as_bytes());
                        ensure!(
                            transcript.len() <= MAX_PENDING,
                            "QEMU serial transcript exceeded {MAX_PENDING} bytes"
                        );
                        if let Some(pattern) = matcher.matching(&transcript, line_start) {
                            bail!("QEMU serial matched global fail_regex `{pattern}`");
                        }
                        if let Some(index) = transcript[previous_length..]
                            .iter()
                            .rposition(|&byte| byte == b'\n')
                        {
                            line_start = previous_length + index + 1;
                        }
                        worker.changed.notify_all();
                        Ok(())
                    };
                    let mut drain_deadline = None;
                    loop {
                        if worker_stop.load(Ordering::Acquire)
                            || worker.cancelled.load(Ordering::Acquire)
                        {
                            let deadline = drain_deadline
                                .get_or_insert_with(|| Instant::now() + Duration::from_secs(5));
                            ensure!(
                                Instant::now() < *deadline,
                                "serial output did not drain after cancellation"
                            );
                        }
                        match stream.read(&mut buffer) {
                            Ok(0) => {
                                consume(&[], true)?;
                                break;
                            }
                            Ok(count) => consume(&buffer[..count], false)?,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::WouldBlock
                                        | io::ErrorKind::TimedOut
                                        | io::ErrorKind::Interrupted
                                ) =>
                            {
                                if worker_stop.load(Ordering::Acquire)
                                    || worker.cancelled.load(Ordering::Acquire)
                                {
                                    consume(&[], true)?;
                                    break;
                                }
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    worker.fail(format!("QEMU serial: {error:#}"));
                }
                worker.state.lock().unwrap().closed = true;
                worker.changed.notify_all();
            });
            Ok((
                Self {
                    shared: shared.clone(),
                    stop,
                    reader: Some(reader),
                },
                Checks {
                    shared,
                    prefix: None,
                    require_newline: false,
                },
            ))
        }

        pub(crate) fn finish(mut self) -> Result<()> {
            self.join();
            self.shared.verdict()
        }

        fn join(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(reader) = self.reader.take()
                && reader.join().is_err()
            {
                self.shared.fail("QEMU serial reader panicked".into());
            }
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            self.join();
        }
    }

    impl Checks {
        pub(crate) fn verdict(&self) -> Result<()> {
            self.shared.verdict()
        }

        pub(crate) fn run(&mut self, steps: &[ShellCheckStep], budget: Duration) -> Result<()> {
            let deadline = Instant::now()
                .checked_add(budget)
                .context("serial check deadline overflow")?;
            for (index, step) in steps.iter().enumerate() {
                self.run_step(step, deadline)
                    .with_context(|| format!("serial shell_check_steps[{index}]"))?;
            }
            Ok(())
        }

        fn run_step(&mut self, step: &ShellCheckStep, deadline: Instant) -> Result<()> {
            if let Some(prefix) = &step.shell_prefix {
                self.prefix = Some(prefix.clone());
            }
            if let Some(command) = &step.shell_cmd {
                let prefix = self
                    .prefix
                    .as_deref()
                    .context("serial command requires a shell prefix")?;
                let require_newline = self.require_newline;
                self.wait(deadline, |text| {
                    Ok(text.match_indices(prefix).find_map(|(start, _)| {
                        (!require_newline || text[..start].ends_with('\n'))
                            .then_some(start + prefix.len())
                    }))
                })?;
                // Keep bytes following the prefix. A guest can stop and return
                // to the host in the same read as its final progress marker;
                // discarding that tail would lose the step's valid result.
                thread::sleep(Duration::from_millis(100));
                let mut bytes = command.trim_end_matches(['\r', '\n']).as_bytes().to_vec();
                bytes.push(b'\n');
                let mut writer = self.shared.writer.lock().unwrap();
                let writer = writer
                    .as_mut()
                    .context("QEMU serial writer is not connected")?;
                for chunk in bytes.chunks(64) {
                    ensure!(
                        !self.shared.cancelled.load(Ordering::Acquire),
                        "serial check cancelled"
                    );
                    writer.write_all(chunk)?;
                    thread::sleep(Duration::from_millis(2));
                }
                writer.flush()?;
            }
            let deadline = match step.timeout {
                Some(seconds) => deadline.min(
                    Instant::now()
                        .checked_add(Duration::from_secs(seconds))
                        .context("serial step deadline overflow")?,
                ),
                None => deadline,
            };
            let successes = step
                .success_regex
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|p| Regex::new(p))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let failures = step
                .fail_regex
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|p| Regex::new(p))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !successes.is_empty() {
                self.wait(deadline, |text| {
                    for failure in &failures {
                        ensure!(!failure.is_match(text), "matched fail_regex `{failure}`");
                    }
                    Ok(successes
                        .iter()
                        .filter_map(|pattern| pattern.find(text).map(|m| m.end()))
                        .min())
                })?;
                self.require_newline = false;
            } else {
                // No extra echo commands: they would alter shell history. The
                // next prompt must follow a newline from the submitted command,
                // rather than an input editor repaint while it is being typed.
                self.require_newline = true;
            }
            Ok(())
        }

        fn wait(
            &self,
            deadline: Instant,
            predicate: impl Fn(&str) -> Result<Option<usize>>,
        ) -> Result<()> {
            let mut state = self.shared.state.lock().unwrap();
            loop {
                if let Some(error) = &state.failure {
                    bail!("{error}");
                }
                ensure!(
                    !self.shared.cancelled.load(Ordering::Acquire),
                    "serial check cancelled"
                );
                let text = state.pending.as_str();
                if let Some(end) = predicate(text)? {
                    state.pending.drain(..end);
                    return Ok(());
                }
                ensure!(
                    !state.closed,
                    "serial console closed before the check completed"
                );
                ensure!(
                    Instant::now() < deadline,
                    "serial check timed out; output tail: {}",
                    &text[text.char_indices().rev().nth(1024).map_or(0, |(i, _)| i)..]
                );
                let wait = POLL.min(deadline.saturating_duration_since(Instant::now()));
                state = self.shared.changed.wait_timeout(state, wait).unwrap().0;
            }
        }
    }

    fn decode_utf8(bytes: &mut Vec<u8>, last: bool) -> String {
        let mut output = String::new();
        let mut consumed = 0;
        while consumed < bytes.len() {
            match std::str::from_utf8(&bytes[consumed..]) {
                Ok(valid) => {
                    output.push_str(valid);
                    consumed = bytes.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.push_str(
                        std::str::from_utf8(&bytes[consumed..consumed + valid])
                            .expect("Utf8Error::valid_up_to identifies a valid UTF-8 prefix"),
                    );
                    consumed += valid;
                    match error.error_len() {
                        Some(length) => {
                            output.push('\u{fffd}');
                            consumed += length;
                        }
                        None if last => {
                            output.push('\u{fffd}');
                            consumed = bytes.len();
                        }
                        None => break,
                    }
                }
            }
        }
        bytes.drain(..consumed);
        output
    }

    #[cfg(test)]
    mod tests {
        use std::{io::BufRead, os::unix::net::UnixListener, sync::mpsc};

        use super::*;

        fn buffered_checks(text: &str) -> Checks {
            Checks {
                shared: Arc::new(Shared {
                    state: Mutex::new(State { pending: text.into(), ..Default::default() }),
                    changed: Condvar::new(),
                    writer: Mutex::new(None),
                    cancelled: Arc::new(AtomicBool::new(false)),
                }),
                prefix: None,
                require_newline: false,
            }
        }

        #[test]
        fn expired_serial_check_cannot_accept_a_late_buffered_marker() {
            let checks = buffered_checks("READY");
            let deadline = Instant::now() - Duration::from_secs(1);
            assert!(checks.wait(deadline, |text| Ok(text.find("READY").map(|i| i + 5))).is_err());
        }

        #[test]
        fn failure_only_step_does_not_silently_ignore_its_failure_pattern() {
            let mut checks = buffered_checks("ERROR\n");
            let result = checks.run(&[ShellCheckStep {
                fail_regex: Some(vec!["ERROR".into()]),
                ..Default::default()
            }], Duration::from_secs(1));
            assert!(result.is_err(), "failure-only checks must observe errors");
        }

        #[test]
        fn serial_checks_preserve_commands_and_monitor_failures_after_completion() {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("console.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (release, wait) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut input = io::BufReader::new(stream.try_clone().unwrap());
                stream.write_all(b"\n\x1b[32mguest> \x1b[0m").unwrap();
                let mut command = String::new();
                input.read_line(&mut command).unwrap();
                assert_eq!(command, "history -c\n");
                stream
                    .write_all(b"\r\x1b[2Kguest> history -c\r\nguest> ")
                    .unwrap();
                command.clear();
                input.read_line(&mut command).unwrap();
                assert_eq!(command, "perform\n");
                stream
                    .write_all(b"perform\r\nnoise\xff\xfe\nRESULT=42\r\nguest> ")
                    .unwrap();
                wait.recv().unwrap();
                stream
                    .write_all(b"\x1b[31mPANIC: late failure\x1b[0m\r\n")
                    .unwrap();
            });
            let (monitor, mut checks) = Monitor::start(
                &socket,
                &[r"(?i)\bPANIC\b".into()],
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
                io::sink(),
            )
            .unwrap();
            checks
                .run(
                    &[
                        ShellCheckStep {
                            shell_prefix: Some("guest>".into()),
                            shell_cmd: Some("history -c".into()),
                            ..Default::default()
                        },
                        ShellCheckStep {
                            shell_cmd: Some("perform".into()),
                            success_regex: Some(vec!["(?m)^RESULT=42$".into()]),
                            ..Default::default()
                        },
                    ],
                    Duration::from_secs(2),
                )
                .unwrap();
            release.send(()).unwrap();
            let error = checks
                .wait(Instant::now() + Duration::from_secs(2), |_| Ok(None))
                .unwrap_err();
            assert!(error.to_string().contains("global fail_regex"), "{error:#}");
            assert!(monitor.finish().is_err());
            server.join().unwrap();
        }

        #[test]
        fn serial_result_after_a_prefix_is_not_discarded_before_sending() {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("console.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"guest>\nALREADY_STOPPED\n").unwrap();
                let mut input = io::BufReader::new(stream);
                let mut command = String::new();
                input.read_line(&mut command).unwrap();
                assert_eq!(command, "detach\n");
            });
            let (monitor, mut checks) = Monitor::start(
                &socket,
                &[],
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
                io::sink(),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut state = checks.shared.state.lock().unwrap();
            while !state.pending.ends_with("ALREADY_STOPPED\n") {
                assert!(Instant::now() < deadline);
                state = checks.shared.changed.wait_timeout(state, POLL).unwrap().0;
            }
            drop(state);
            checks
                .run(
                    &[ShellCheckStep {
                        shell_prefix: Some("guest>".into()),
                        shell_cmd: Some("detach".into()),
                        success_regex: Some(vec!["(?m)^ALREADY_STOPPED$".into()]),
                        ..Default::default()
                    }],
                    Duration::from_secs(1),
                )
                .unwrap();
            monitor.finish().unwrap();
            server.join().unwrap();
        }

        #[test]
        fn finish_drains_queued_serial_errors_after_the_last_check() {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("console.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (release, wait) = mpsc::channel();
            let (written, queued) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut input = io::BufReader::new(stream.try_clone().unwrap());
                stream.write_all(b"ready>").unwrap();
                let mut command = String::new();
                input.read_line(&mut command).unwrap();
                assert_eq!(command, "ping\n");
                stream.write_all(b"OK\n").unwrap();
                wait.recv().unwrap();
                let mut tail = vec![b'x'; 16384];
                tail.extend_from_slice(b"\nPANIC: queued after completion\n");
                stream.write_all(&tail).unwrap();
                written.send(()).unwrap();
            });
            let (monitor, mut checks) = Monitor::start(
                &socket,
                &["PANIC:".into()],
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
                io::sink(),
            )
            .unwrap();
            checks
                .run(
                    &[ShellCheckStep {
                        shell_prefix: Some("ready>".into()),
                        shell_cmd: Some("ping".into()),
                        success_regex: Some(vec!["OK".into()]),
                        ..Default::default()
                    }],
                    Duration::from_secs(2),
                )
                .unwrap();
            let state = checks.shared.state.lock().unwrap();
            release.send(()).unwrap();
            queued.recv().unwrap();
            monitor.stop.store(true, Ordering::Release);
            drop(state);
            assert!(
                monitor.finish().is_err(),
                "queued failure must not be lost during shutdown"
            );
            server.join().unwrap();
        }

        #[test]
        fn serial_step_failure_wins_over_a_success_in_the_same_output() {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("console.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"guest> ").unwrap();
                let mut input = io::BufReader::new(stream.try_clone().unwrap());
                let mut command = String::new();
                input.read_line(&mut command).unwrap();
                assert_eq!(command, "perform\n");
                stream.write_all(b"\r\nERROR\nRESULT=42\n").unwrap();
            });
            let (monitor, mut checks) = Monitor::start(
                &socket,
                &[],
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
                io::sink(),
            )
            .unwrap();
            let error = checks
                .run(
                    &[ShellCheckStep {
                        shell_prefix: Some("guest>".into()),
                        shell_cmd: Some("perform".into()),
                        success_regex: Some(vec!["(?m)^RESULT=42$".into()]),
                        fail_regex: Some(vec!["(?m)^ERROR$".into()]),
                        ..Default::default()
                    }],
                    Duration::from_secs(2),
                )
                .unwrap_err();
            assert!(format!("{error:#}").contains("matched fail_regex"));
            monitor.finish().unwrap();
            server.join().unwrap();
        }
    }
}

#[cfg(unix)]
pub(super) use unix::Monitor as SerialMonitor;

#[cfg(not(unix))]
pub(super) struct SerialMonitor;
#[cfg(not(unix))]
pub(super) struct SerialChecks;

#[cfg(not(unix))]
impl SerialMonitor {
    pub(super) fn start(
        _: &Path,
        _: &[String],
        _: Arc<std::sync::atomic::AtomicBool>,
        _: Duration,
        _: impl std::io::Write + Send + 'static,
    ) -> Result<(Self, SerialChecks)> {
        anyhow::bail!("mixed serial/HTTP checks require a Unix serial socket")
    }
    pub(super) fn finish(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(not(unix))]
impl SerialChecks {
    pub(super) fn verdict(&self) -> Result<()> {
        Ok(())
    }
    pub(super) fn run(&mut self, _: &[ShellCheckStep], _: Duration) -> Result<()> {
        anyhow::bail!("mixed serial/HTTP checks require a Unix serial socket")
    }
}

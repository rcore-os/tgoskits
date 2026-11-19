//! Ordered checks over a QEMU serial socket while HTTP checks share its lifetime.

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use ostool::run::ShellCheckStep;

#[cfg(unix)]
mod failures;

#[cfg(unix)]
mod unix {
    use std::{
        collections::VecDeque,
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
        received: VecDeque<(usize, Instant)>,
        failure: Option<String>,
        closed: bool,
        connected_at: Option<Instant>,
    }

    impl State {
        fn append(&mut self, text: &str, at: Instant) {
            if !text.is_empty() {
                self.pending.push_str(text);
                self.received.push_back((self.pending.len(), at));
            }
        }

        fn visible_before(&self, deadline: Instant) -> &str {
            let end = self
                .received
                .iter()
                .take_while(|(_, at)| *at <= deadline)
                .last()
                .map_or(0, |(end, _)| *end);
            &self.pending[..end]
        }

        fn drain(&mut self, end: usize) {
            self.pending.drain(..end);
            while self
                .received
                .front()
                .is_some_and(|(offset, _)| *offset <= end)
            {
                self.received.pop_front();
            }
            for (offset, _) in &mut self.received {
                *offset -= end;
            }
        }
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
        active_failures: Vec<Regex>,
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
                    worker.state.lock().unwrap().connected_at = Some(Instant::now());
                    worker.changed.notify_all();
                    let mut buffer = [0u8; 8192];
                    let mut escape = 0u8;
                    let mut transcript = Vec::new();
                    let mut line_start = 0;
                    let mut utf8 = Vec::new();
                    let mut consume =
                        |bytes: &[u8], last: bool, received_at: Instant| -> Result<()> {
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
                            state.append(&decoded, received_at);
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
                                consume(&[], true, Instant::now())?;
                                break;
                            }
                            Ok(count) => consume(&buffer[..count], false, Instant::now())?,
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
                                    consume(&[], true, Instant::now())?;
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
                    active_failures: Vec::new(),
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
        pub(crate) fn connected_at(&self) -> Result<Instant> {
            let mut state = self.shared.state.lock().unwrap();
            loop {
                if let Some(error) = &state.failure {
                    bail!("{error}");
                }
                ensure!(
                    !self.shared.cancelled.load(Ordering::Acquire) && !state.closed,
                    "serial connection cancelled"
                );
                if let Some(at) = state.connected_at {
                    return Ok(at);
                }
                state = self.shared.changed.wait_timeout(state, POLL).unwrap().0;
            }
        }
        pub(crate) fn verdict(&self) -> Result<()> {
            self.shared.verdict()?;
            self.check_failures(&self.shared.state.lock().unwrap().pending)
        }

        #[cfg(test)]
        pub(crate) fn run(&mut self, steps: &[ShellCheckStep], budget: Duration) -> Result<()> {
            let deadline = Instant::now()
                .checked_add(budget)
                .context("serial check deadline overflow")?;
            self.run_until(steps, deadline)
        }

        pub(crate) fn run_until(
            &mut self,
            steps: &[ShellCheckStep],
            deadline: Instant,
        ) -> Result<()> {
            for (index, step) in steps.iter().enumerate() {
                self.run_step(step, deadline)
                    .with_context(|| format!("serial shell_check_steps[{index}]"))?;
            }
            self.verdict()
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
                ensure!(
                    Instant::now() < deadline,
                    "serial member deadline expired before command"
                );
                thread::sleep(
                    Duration::from_millis(100)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
                ensure!(
                    Instant::now() < deadline,
                    "serial member deadline expired before command"
                );
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
                    ensure!(
                        Instant::now() < deadline,
                        "serial member deadline expired while sending command"
                    );
                    writer.set_write_timeout(Some(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .max(Duration::from_millis(1)),
                    ))?;
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
            self.active_failures = failures;
            self.check_failures(&self.shared.state.lock().unwrap().pending)?;
            if !successes.is_empty() {
                self.wait(deadline, |text| {
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
                if !self.active_failures.is_empty() {
                    let prefix = self.prefix.clone().context(
                        "failure-only step requires a shell prompt to observe completion",
                    )?;
                    self.wait(deadline, |text| {
                        Ok(text.match_indices(&prefix).find_map(|(start, _)| {
                            text[..start]
                                .ends_with('\n')
                                .then_some(start + prefix.len())
                        }))
                    })?;
                    self.require_newline = false;
                }
            }
            Ok(())
        }

        fn check_failures(&self, text: &str) -> Result<()> {
            for pattern in &self.active_failures {
                ensure!(!pattern.is_match(text), "matched fail_regex `{pattern}`");
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
                self.check_failures(&state.pending)?;
                let text = state.visible_before(deadline);
                if let Some(end) = predicate(text)? {
                    state.drain(end);
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
    mod tests;
}

#[cfg(unix)]
pub(crate) use unix::{Checks as SerialChecks, Monitor as SerialMonitor};

#[cfg(not(unix))]
pub(crate) struct SerialMonitor;
#[cfg(not(unix))]
pub(crate) struct SerialChecks;

#[cfg(not(unix))]
impl SerialMonitor {
    pub(crate) fn start(
        _: &Path,
        _: &[String],
        _: Arc<std::sync::atomic::AtomicBool>,
        _: Duration,
        _: impl std::io::Write + Send + 'static,
    ) -> Result<(Self, SerialChecks)> {
        anyhow::bail!("mixed serial/HTTP checks require a Unix serial socket")
    }
    pub(crate) fn finish(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(not(unix))]
impl SerialChecks {
    pub(crate) fn connected_at(&self) -> Result<std::time::Instant> {
        anyhow::bail!("ordered serial checks require a Unix serial socket")
    }
    pub(crate) fn verdict(&self) -> Result<()> {
        Ok(())
    }
    pub(crate) fn run_until(&mut self, _: &[ShellCheckStep], _: std::time::Instant) -> Result<()> {
        anyhow::bail!("bounded serial checks require a Unix serial socket")
    }
    pub(crate) fn run(&mut self, _: &[ShellCheckStep], _: Duration) -> Result<()> {
        anyhow::bail!("mixed serial/HTTP checks require a Unix serial socket")
    }
}

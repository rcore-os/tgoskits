use alloc::{collections::VecDeque, sync::Arc, task::Wake, vec::Vec};
use core::{
    future::poll_fn,
    marker::PhantomData,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    task::{Poll, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_task::future::block_on;
use axpoll::{IoEvents, PollSet};
use linux_raw_sys::general::{
    ECHOCTL, ECHOK, ICRNL, IGNCR, ISIG, ONLCR, OPOST, VEOF, VERASE, VKILL, VMIN, VTIME,
};
use ringbuf::{
    CachingCons, CachingProd,
    traits::{Consumer, Observer, Producer, Split},
};
use starry_signal::SignalInfo;

use super::{Terminal, termios::Termios2};
use crate::task::send_signal_to_process_group;

const BUF_SIZE: usize = 80;

type ReadBuf = Arc<ringbuf::StaticRb<u8, BUF_SIZE>>;

/// How should we process inputs?
pub enum ProcessMode {
    /// Process inputs without an external event source.
    ///
    /// This is used as the fallback for consoles without an RX interrupt. A
    /// background task drains input directly and yields when idle, so signals
    /// and serial auto-init commands still work while no user task is blocked
    /// in `read()`.
    Manual,
    /// Spawns task for processing inputs, relying on an external event source
    /// to wake it up.
    InterruptDriven(Arc<PollSet>),
    /// Do not process inputs.
    ///
    /// This is only used by the master side of pseudo tty. The argument is the
    /// [`PollSet`] for incoming data.
    Passive(Arc<PollSet>),
}

pub struct TtyConfig<R, W> {
    pub reader: R,
    pub writer: W,
    pub process_mode: ProcessMode,
}

pub trait TtyRead: Send + Sync + 'static {
    fn read(&mut self, buf: &mut [u8]) -> usize;
    /// Whether the writer peer has been fully closed. Default: never closed.
    fn closed(&self) -> bool {
        false
    }
}
pub trait TtyWrite: Send + Sync + 'static {
    fn write(&self, buf: &[u8]);
    /// Notify that the writer side has been fully closed (last fd). Default: no-op.
    fn close(&self) {}
}

pub fn write_output_bytes<W: TtyWrite + ?Sized>(writer: &W, term: &Termios2, buf: &[u8]) {
    if !term.has_oflag(OPOST) || !term.has_oflag(ONLCR) {
        writer.write(buf);
        return;
    }

    // Collect output with \n -> \r\n translation into a single buffer so the
    // underlying writer sees exactly one call per write_output_bytes() instead
    // of one call per newline.  A TUI frame typically contains many cursor-
    // movement newlines; the per-newline approach forced the UART driver to
    // acquire/release its lock dozens of times for a single frame and made
    // terminal emulators render the frame line-by-line (visible flicker).
    let extra = buf.iter().filter(|&&b| b == b'\n').count();
    if extra == 0 {
        writer.write(buf);
        return;
    }
    let mut out = alloc::vec::Vec::with_capacity(buf.len() + extra);
    for &byte in buf {
        if byte == b'\n' {
            out.push(b'\r');
        }
        out.push(byte);
    }
    writer.write(&out);
}

struct InputReader<R, W> {
    terminal: Arc<Terminal>,

    reader: R,
    writer: W,

    buf_tx: CachingProd<ReadBuf>,
    read_buf: [u8; BUF_SIZE],
    read_range: Range<usize>,

    line_buf: Vec<u8>,
    line_read: Option<usize>,
    eof_ready: Arc<AtomicBool>,
    clear_line_buf: Arc<AtomicBool>,
}
impl<R: TtyRead, W: TtyWrite> InputReader<R, W> {
    pub fn drain_source_into_line_buffer(&mut self) -> bool {
        if self.clear_line_buf.swap(false, Ordering::Relaxed) {
            self.line_buf.clear();
        }
        let mut progressed = false;
        if self.read_range.is_empty() {
            let read = self.reader.read(&mut self.read_buf);
            self.read_range = 0..read;
            progressed |= read > 0;
        }
        let term = self.terminal.load_termios();
        let mut sent = 0;
        loop {
            if let Some(offset) = &mut self.line_read {
                let read = self.buf_tx.push_slice(&self.line_buf[*offset..]);
                if read == 0 {
                    break;
                }
                sent += read;
                *offset += read;
                if *offset == self.line_buf.len() {
                    self.line_read = None;
                    self.line_buf.clear();
                }
                continue;
            }
            if self.buf_tx.is_full() || self.read_range.is_empty() {
                break;
            }
            let mut ch = self.read_buf[self.read_range.start];
            self.read_range.start += 1;
            progressed = true;

            if ch == b'\r' {
                if term.has_iflag(IGNCR) {
                    continue;
                }
                if term.has_iflag(ICRNL) {
                    ch = b'\n';
                }
            }

            let signaled = self.check_send_signal(&term, ch);

            let eof = term.canonical() && ch == term.special_char(VEOF);
            if term.echo() && !eof {
                self.output_char(&term, ch);
            }
            if signaled {
                self.line_buf.clear();
                self.line_read = None;
                continue;
            }
            if !term.canonical() {
                self.buf_tx.try_push(ch).unwrap();
                sent += 1;
                continue;
            }

            // Canonical mode
            if term.has_lflag(ECHOK) && ch == term.special_char(VKILL) {
                self.line_buf.clear();
                continue;
            }
            if ch == term.special_char(VERASE) {
                self.line_buf.pop();
                continue;
            }

            if ch == term.special_char(VEOF) {
                if self.line_buf.is_empty() {
                    self.eof_ready.store(true, Ordering::Release);
                    sent += 1;
                } else {
                    self.line_read = Some(0);
                }
                continue;
            }

            if term.is_eol(ch) {
                self.line_buf.push(ch);
                self.line_read = Some(0);
                continue;
            }

            if ch == b'\t' || !ch.is_ascii_control() {
                self.line_buf.push(ch);
                continue;
            }
        }

        sent > 0 || progressed
    }

    fn check_send_signal(&self, term: &Termios2, ch: u8) -> bool {
        if !term.has_lflag(ISIG) {
            return false;
        }
        if let Some(signo) = term.signo_for(ch) {
            if let Some(pg) = self.terminal.job_control.foreground() {
                let sig = SignalInfo::new_kernel(signo);
                if let Err(err) = send_signal_to_process_group(pg.pgid(), Some(sig)) {
                    warn!("Failed to send signal: {err:?}");
                }
            }
            true
        } else {
            false
        }
    }

    fn output_char(&self, term: &Termios2, ch: u8) {
        match ch {
            b'\n' => write_output_bytes(&self.writer, term, b"\n"),
            b'\t' => self.writer.write(b"\t"),
            ch if ch == term.special_char(VERASE) => self.writer.write(b"\x08 \x08"),
            ch if ch == b' ' || ch.is_ascii_graphic() || !ch.is_ascii() => {
                self.writer.write(&[ch]);
            }
            ch if ch.is_ascii_control() && term.has_lflag(ECHOCTL) => {
                let escaped = if ch == b'\x7f' { b'?' } else { ch + 0x40 };
                self.writer.write(&[b'^', escaped]);
            }
            ch if ch.is_ascii_control() => {
                self.writer.write(&[ch]);
            }
            other => {
                warn!("Ignored echo char: {other:#x}");
            }
        }
    }
}

struct SimpleReader<R> {
    reader: R,
    read_buf: [u8; BUF_SIZE],
    read_range: Range<usize>,
    buf_tx: CachingProd<ReadBuf>,
}
impl<R: TtyRead> SimpleReader<R> {
    pub fn poll(&mut self) {
        while !self.buf_tx.is_full() {
            if self.read_range.is_empty() {
                let read = self.reader.read(&mut self.read_buf);
                if read == 0 {
                    break;
                }
                self.read_range = 0..read;
            }
            let written = self
                .buf_tx
                .push_slice(&self.read_buf[self.read_range.clone()]);
            if written == 0 {
                break;
            }
            self.read_range.start += written;
        }
    }

    pub fn closed(&self) -> bool {
        self.reader.closed()
    }
}

enum Processor<R> {
    InterruptDriven,
    Passive(SimpleReader<R>, Arc<PollSet>),
}

pub struct LineDiscipline<R, W> {
    terminal: Arc<Terminal>,
    buf_rx: CachingCons<ReadBuf>,
    injected_input: VecDeque<u8>,
    input_ready: Arc<PollSet>,
    pump_retry: Arc<PollSet>,
    eof_ready: Arc<AtomicBool>,
    clear_line_buf: Arc<AtomicBool>,
    processor: Processor<R>,
    _writer: PhantomData<W>,
}

struct WakeSignal {
    fired: Arc<AtomicBool>,
    task: Waker,
}

impl Wake for WakeSignal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.fired.store(true, Ordering::Release);
        self.task.wake_by_ref();
    }
}

impl<R: TtyRead, W: TtyWrite> LineDiscipline<R, W> {
    fn drive_input(reader: &mut InputReader<R, W>, input_ready: &PollSet) -> bool {
        let mut progressed = false;
        while reader.drain_source_into_line_buffer() {
            progressed = true;
            // New line-discipline input is visible before waking readers.
            unsafe { input_ready.wake(IoEvents::IN) };
        }
        progressed
    }

    fn spawn_interrupt_driven_reader(
        mut reader: InputReader<R, W>,
        input_source: Arc<PollSet>,
        input_ready: Arc<PollSet>,
        pump_retry: Arc<PollSet>,
    ) {
        ax_task::spawn_with_name(
            move || loop {
                Self::drive_input(&mut reader, input_ready.as_ref());

                let fired = Arc::new(AtomicBool::new(false));
                block_on(poll_fn(|cx| {
                    if Self::drive_input(&mut reader, input_ready.as_ref())
                        || fired.swap(false, Ordering::AcqRel)
                    {
                        return Poll::Ready(());
                    }

                    let waker = Waker::from(Arc::new(WakeSignal {
                        fired: fired.clone(),
                        task: cx.waker().clone(),
                    }));
                    // The reader task registers from ordinary task context.
                    unsafe { input_source.register(&waker, IoEvents::IN) };
                    unsafe { pump_retry.register(&waker, IoEvents::OUT) };

                    if Self::drive_input(&mut reader, input_ready.as_ref())
                        || fired.swap(false, Ordering::AcqRel)
                    {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }));
            },
            "tty-reader".into(),
        );
    }

    fn spawn_polling_reader(mut reader: InputReader<R, W>, input_ready: Arc<PollSet>) {
        ax_task::spawn_with_name(
            move || loop {
                if !Self::drive_input(&mut reader, input_ready.as_ref()) {
                    ax_task::yield_now();
                }
            },
            "tty-poll-reader".into(),
        );
    }

    pub fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Self {
        let (buf_tx, buf_rx) = ReadBuf::default().split();

        let eof_ready = Arc::new(AtomicBool::new(false));
        let clear_line_buf = Arc::new(AtomicBool::new(false));
        let reader = InputReader {
            terminal: terminal.clone(),

            reader: config.reader,
            writer: config.writer,

            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,

            line_buf: Vec::new(),
            line_read: None,
            eof_ready: eof_ready.clone(),
            clear_line_buf: clear_line_buf.clone(),
        };

        let input_ready = Arc::new(PollSet::new());
        let pump_retry = Arc::new(PollSet::new());
        let processor = match config.process_mode {
            ProcessMode::InterruptDriven(input_source) => {
                Self::spawn_interrupt_driven_reader(
                    reader,
                    input_source,
                    input_ready.clone(),
                    pump_retry.clone(),
                );
                Processor::InterruptDriven
            }
            ProcessMode::Manual => {
                Self::spawn_polling_reader(reader, input_ready.clone());
                Processor::InterruptDriven
            }
            ProcessMode::Passive(poll_rx) => {
                let InputReader { reader, buf_tx, .. } = reader;
                Processor::Passive(
                    SimpleReader {
                        reader,
                        read_buf: [0; BUF_SIZE],
                        read_range: 0..0,
                        buf_tx,
                    },
                    poll_rx,
                )
            }
        };
        Self {
            terminal,
            buf_rx,
            injected_input: VecDeque::new(),
            input_ready,
            pump_retry,
            eof_ready,
            clear_line_buf,
            processor,
            _writer: PhantomData,
        }
    }

    pub fn drain_input(&mut self) {
        self.buf_rx.clear();
        self.injected_input.clear();
        self.eof_ready.store(false, Ordering::Release);
        self.clear_line_buf.store(true, Ordering::Relaxed);
    }

    pub fn inject_input(&mut self, input: &[u8]) {
        self.injected_input.extend(input);
        // Injected bytes are visible before waking readers.
        unsafe { self.input_ready.wake(IoEvents::IN) };
    }

    pub fn poll_read(&mut self) -> bool {
        if let Processor::Passive(reader, _) = &mut self.processor {
            reader.poll();
            if reader.closed() {
                return true;
            }
        }
        if !self.injected_input.is_empty() {
            return true;
        }
        let term = self.terminal.termios.lock().clone();
        if term.canonical() {
            return self.eof_ready.load(Ordering::Acquire) || !self.buf_rx.is_empty();
        }
        // VMIN=0 means read() returns immediately with 0 bytes if empty, but
        // poll() should still only report POLLIN when actual data is present.
        // This matches Linux n_tty behavior: minimum_chars_to_read() treats
        // VMIN=0 as requiring at least 1 byte to wake poll().
        let vmin = term.special_char(VMIN) as usize;
        !self.buf_rx.is_empty() && (vmin == 0 || self.buf_rx.occupied_len() >= vmin)
    }

    pub fn register_rx_waker(&self, waker: &Waker) {
        match &self.processor {
            Processor::InterruptDriven => {
                // Registration happens from tty read poll context.
                unsafe { self.input_ready.register(waker, IoEvents::IN) };
            }
            Processor::Passive(_, set) => {
                // Registration happens from tty read poll context.
                unsafe { set.register(waker, IoEvents::IN) };
            }
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.injected_input.is_empty() {
            let mut read = 0;
            for slot in buf.iter_mut() {
                if let Some(byte) = self.injected_input.pop_front() {
                    *slot = byte;
                    read += 1;
                } else {
                    break;
                }
            }
            return Ok(read);
        }
        if matches!(self.processor, Processor::Passive(_, _)) {
            let closed = match &self.processor {
                Processor::Passive(reader, _) => reader.closed(),
                _ => unreachable!(),
            };
            let read = self.buf_rx.pop_slice(buf);
            return if read == 0 {
                if closed {
                    Ok(0)
                } else {
                    Err(AxError::WouldBlock)
                }
            } else {
                Ok(read)
            };
        }

        let term = self.terminal.termios.lock().clone();
        let vmin = if term.canonical() {
            1
        } else {
            let vtime = term.special_char(VTIME);
            if vtime > 0 {
                todo!();
            }
            term.special_char(VMIN) as usize
        };

        if buf.len() < vmin {
            return Err(AxError::WouldBlock);
        }

        let available = self.buf_rx.occupied_len();
        if available == 0 {
            if term.canonical() && self.eof_ready.swap(false, Ordering::AcqRel) {
                return Ok(0);
            }
            if vmin == 0 {
                return Ok(0);
            }
            return Err(AxError::WouldBlock);
        }
        if vmin > 0 && available < vmin {
            return Err(AxError::WouldBlock);
        }

        let read = self.buf_rx.pop_slice(buf);
        // Buffer space was freed before waking the input pump.
        unsafe { self.pump_retry.wake(IoEvents::OUT) };
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::sync::atomic::AtomicBool;

    use axpoll::PollSet;
    use ringbuf::traits::{Observer, Split};

    use super::{
        BUF_SIZE, InputReader, LineDiscipline, ProcessMode, ReadBuf, TtyConfig, TtyRead, TtyWrite,
    };
    use crate::pseudofs::dev::tty::terminal::Terminal;

    struct MockReader {
        data: Vec<u8>,
        pos: usize,
    }
    impl MockReader {
        fn new(data: Vec<u8>) -> Self {
            Self { data, pos: 0 }
        }
    }
    impl TtyRead for MockReader {
        fn read(&mut self, buf: &mut [u8]) -> usize {
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            n
        }
    }

    struct MockWriter;
    impl TtyWrite for MockWriter {
        fn write(&self, _buf: &[u8]) {}
    }

    fn make_reader(
        data: Vec<u8>,
    ) -> (
        InputReader<MockReader, MockWriter>,
        ringbuf::CachingCons<ReadBuf>,
    ) {
        let (buf_tx, buf_rx) = ReadBuf::default().split();
        let reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(data),
            writer: MockWriter,
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };
        (reader, buf_rx)
    }

    /// Regression test: a canonical-mode input longer than BUF_SIZE characters
    /// (with no newline in the first chunk) must not stall drain_source_into_line_buffer.
    ///
    /// Before the fix, the function returned `sent > 0` which was false after the
    /// first BUF_SIZE bytes were consumed into line_buf (no newline yet), causing
    /// drive_input() to stop looping and the remaining input (including the newline)
    /// to be silently dropped.  The board CI symptom was shell commands being
    /// truncated to the first BUF_SIZE characters (e.g. "sleep 5; ..." → "leep 5; ...").
    #[test]
    fn canonical_long_line_drain_continues_past_buf_size() {
        // BUF_SIZE ordinary chars followed by '\n' — total BUF_SIZE+1 bytes.
        let mut data: Vec<u8> = (0..BUF_SIZE).map(|_| b'a').collect();
        data.push(b'\n');

        let (mut reader, rx) = make_reader(data);

        // First drain: reads the BUF_SIZE 'a' bytes into line_buf; no newline yet,
        // so nothing reaches buf_rx.  Must still return true (progress was made).
        assert!(
            reader.drain_source_into_line_buffer(),
            "first drain must return true even though buf_rx is still empty"
        );
        assert_eq!(
            rx.occupied_len(),
            0,
            "buf_rx must remain empty before the newline is processed"
        );

        // Second drain: reads the '\n', completes the line, flushes to buf_rx.
        reader.drain_source_into_line_buffer();
        assert!(
            rx.occupied_len() > 0,
            "buf_rx must contain data after the newline is processed"
        );
    }

    #[test]
    fn injected_input_is_readable_immediately() {
        let mut ldisc = LineDiscipline::new(
            Arc::new(Terminal::default()),
            TtyConfig {
                reader: MockReader::new(Vec::new()),
                writer: MockWriter,
                process_mode: ProcessMode::Passive(Arc::new(PollSet::new())),
            },
        );

        ldisc.inject_input(b"\x1b[1;1R");

        assert!(ldisc.poll_read(), "injected bytes must make tty readable");

        let mut buf = [0; 6];
        assert_eq!(ldisc.read(&mut buf), Ok(6));
        assert_eq!(&buf, b"\x1b[1;1R");
    }
}

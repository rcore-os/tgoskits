use alloc::{boxed::Box, collections::VecDeque, sync::Arc, task::Wake, vec::Vec};
use core::{
    future::poll_fn,
    marker::PhantomData,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Poll, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
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

const BUF_SIZE: usize = 4096;
const ECHO_QUEUE_CAP: usize = 4096;
const ECHO_WRITE_CHUNK: usize = 256;

type ReadBuf = Arc<ringbuf::StaticRb<u8, BUF_SIZE>>;

/// How should we process inputs?
pub enum ProcessMode {
    /// Spawns task for processing inputs, relying on an external event source
    /// to wake it up.
    InterruptDriven {
        input: Arc<PollSet>,
        output: Option<Arc<PollSet>>,
    },
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
}
pub trait TtyWrite: Send + Sync + 'static {
    fn open(&self) -> AxResult<()> {
        Ok(())
    }

    fn write(&self, buf: &[u8]);

    fn try_write(&self, buf: &[u8]) -> usize {
        self.write(buf);
        buf.len()
    }

    fn flush_echo_before_input(&self) -> bool {
        false
    }

    fn max_sync_echo_bytes(&self) -> usize {
        if self.flush_echo_before_input() {
            usize::MAX
        } else {
            0
        }
    }

    fn drain(&self) -> AxResult<()> {
        Ok(())
    }

    fn termios_changed(&self, _old: &Termios2, _new: &Termios2) {}
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
    echo: Arc<EchoQueue<W>>,

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
        let mut echo = Vec::new();
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
                Self::append_echo_char(&term, ch, &mut echo);
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

        if !echo.is_empty() {
            if echo.len() <= self.echo.max_sync_bytes() {
                self.echo.write_now(&echo);
            } else {
                self.echo.enqueue(&echo);
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

    fn append_echo_char(term: &Termios2, ch: u8, out: &mut Vec<u8>) {
        match ch {
            b'\n' if term.has_oflag(OPOST) && term.has_oflag(ONLCR) => {
                out.extend_from_slice(b"\r\n");
            }
            b'\n' => out.push(b'\n'),
            b'\t' => out.push(b'\t'),
            ch if ch == term.special_char(VERASE) => out.extend_from_slice(b"\x08 \x08"),
            ch if ch == b' ' || ch.is_ascii_graphic() || !ch.is_ascii() => {
                out.push(ch);
            }
            ch if ch.is_ascii_control() && term.has_lflag(ECHOCTL) => {
                let escaped = if ch == b'\x7f' { b'?' } else { ch + 0x40 };
                out.extend_from_slice(&[b'^', escaped]);
            }
            ch if ch.is_ascii_control() => {
                out.push(ch);
            }
            other => {
                warn!("Ignored echo char: {other:#x}");
            }
        }
    }
}

struct EchoQueue<W> {
    writer: W,
    queue: SpinNoIrq<VecDeque<u8>>,
    wake_source: Arc<PollSet>,
    dropped: AtomicUsize,
}

impl<W: TtyWrite> EchoQueue<W> {
    fn new(writer: W, wake_source: Arc<PollSet>) -> Arc<Self> {
        Arc::new(Self {
            writer,
            queue: SpinNoIrq::new(VecDeque::new()),
            wake_source,
            dropped: AtomicUsize::new(0),
        })
    }

    fn enqueue(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        let queued = {
            let mut queue = self.queue.lock();
            let space = ECHO_QUEUE_CAP.saturating_sub(queue.len());
            let queued = bytes.len().min(space);
            queue.extend(bytes[..queued].iter().copied());
            queued
        };

        if queued < bytes.len() {
            self.dropped
                .fetch_add(bytes.len() - queued, Ordering::AcqRel);
        }
        unsafe { self.wake_source.wake(IoEvents::OUT) };
    }

    fn max_sync_bytes(&self) -> usize {
        self.writer.max_sync_echo_bytes()
    }

    fn write_now(&self, bytes: &[u8]) {
        let written = self.writer.try_write(bytes);
        if written < bytes.len() {
            self.enqueue(&bytes[written..]);
        }
    }

    fn drain_available(&self) -> bool {
        let mut progressed = false;
        loop {
            let chunk = {
                let queue = self.queue.lock();
                if queue.is_empty() {
                    break;
                }
                let len = queue.len().min(ECHO_WRITE_CHUNK);
                let mut chunk = Vec::with_capacity(len);
                for byte in queue.iter().take(len) {
                    chunk.push(*byte);
                }
                chunk
            };
            let written = self.writer.try_write(&chunk);
            if written == 0 {
                break;
            }
            {
                let mut queue = self.queue.lock();
                for _ in 0..written {
                    if queue.pop_front().is_none() {
                        break;
                    }
                }
            }
            progressed = true;
        }

        let dropped = self.dropped.swap(0, Ordering::AcqRel);
        if dropped > 0 {
            warn!("Dropped {dropped} tty echo byte(s)");
            progressed = true;
        }
        progressed
    }
}

struct SimpleReader<R> {
    reader: R,
    read_buf: [u8; BUF_SIZE],
    buf_tx: CachingProd<ReadBuf>,
}
impl<R: TtyRead> SimpleReader<R> {
    pub fn poll(&mut self) {
        let read = self.reader.read(&mut self.read_buf);
        let _ = self.buf_tx.push_slice(&self.read_buf[..read]);
    }
}

enum Processor<R> {
    InterruptDriven,
    Passive(Box<SimpleReader<R>>, Arc<PollSet>),
}

pub struct LineDiscipline<R, W> {
    terminal: Arc<Terminal>,
    buf_rx: CachingCons<ReadBuf>,
    injected_input: VecDeque<u8>,
    input_ready: Arc<PollSet>,
    worker_source: Arc<PollSet>,
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
        progressed |= reader.echo.drain_available();
        while reader.drain_source_into_line_buffer() {
            progressed = true;
            reader.echo.drain_available();
            // New line-discipline input is visible before waking readers.
            unsafe { input_ready.wake(IoEvents::IN) };
        }
        progressed |= reader.echo.drain_available();
        progressed
    }

    fn spawn_interrupt_driven_reader(
        mut reader: InputReader<R, W>,
        input_source: Arc<PollSet>,
        output_source: Option<Arc<PollSet>>,
        input_ready: Arc<PollSet>,
        worker_source: Arc<PollSet>,
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
                    if let Some(output_source) = output_source.as_ref() {
                        unsafe { output_source.register(&waker, IoEvents::OUT) };
                    }
                    unsafe { worker_source.register(&waker, IoEvents::OUT) };

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

    pub fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Self {
        let (buf_tx, buf_rx) = ReadBuf::default().split();

        let eof_ready = Arc::new(AtomicBool::new(false));
        let clear_line_buf = Arc::new(AtomicBool::new(false));
        let input_ready = Arc::new(PollSet::new());
        let worker_source = Arc::new(PollSet::new());
        let echo = EchoQueue::new(config.writer, worker_source.clone());
        let reader = InputReader {
            terminal: terminal.clone(),

            reader: config.reader,
            echo: echo.clone(),

            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,

            line_buf: Vec::new(),
            line_read: None,
            eof_ready: eof_ready.clone(),
            clear_line_buf: clear_line_buf.clone(),
        };

        let processor = match config.process_mode {
            ProcessMode::InterruptDriven { input, output } => {
                Self::spawn_interrupt_driven_reader(
                    reader,
                    input,
                    output,
                    input_ready.clone(),
                    worker_source.clone(),
                );
                Processor::InterruptDriven
            }
            ProcessMode::Passive(poll_rx) => {
                let InputReader { reader, buf_tx, .. } = reader;
                Processor::Passive(
                    Box::new(SimpleReader {
                        reader,
                        read_buf: [0; BUF_SIZE],
                        buf_tx,
                    }),
                    poll_rx,
                )
            }
        };
        Self {
            terminal,
            buf_rx,
            injected_input: VecDeque::new(),
            input_ready,
            worker_source,
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
            let read = self.buf_rx.pop_slice(buf);
            return if read == 0 {
                Err(AxError::WouldBlock)
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
        unsafe { self.worker_source.wake(IoEvents::OUT) };
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use axpoll::PollSet;
    use ringbuf::traits::{Observer, Split};

    use super::{
        BUF_SIZE, EchoQueue, InputReader, LineDiscipline, ProcessMode, ReadBuf, TtyConfig, TtyRead,
        TtyWrite,
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

    struct CountingWriter {
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
    }

    impl TtyWrite for CountingWriter {
        fn write(&self, buf: &[u8]) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        }

        fn try_write(&self, buf: &[u8]) -> usize {
            self.write(buf);
            buf.len()
        }
    }

    struct OrderedEchoWriter {
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
    }

    impl TtyWrite for OrderedEchoWriter {
        fn write(&self, buf: &[u8]) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        }

        fn try_write(&self, buf: &[u8]) -> usize {
            self.write(buf);
            buf.len()
        }

        fn flush_echo_before_input(&self) -> bool {
            true
        }
    }

    struct LimitedEchoWriter {
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
        limit: usize,
    }

    impl TtyWrite for LimitedEchoWriter {
        fn write(&self, buf: &[u8]) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        }

        fn try_write(&self, buf: &[u8]) -> usize {
            self.write(buf);
            buf.len()
        }

        fn max_sync_echo_bytes(&self) -> usize {
            self.limit
        }
    }

    struct BackpressuredWriter {
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
        budget: Arc<AtomicUsize>,
    }

    impl TtyWrite for BackpressuredWriter {
        fn write(&self, _buf: &[u8]) {
            panic!("echo flushing must use non-blocking try_write");
        }

        fn try_write(&self, buf: &[u8]) -> usize {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let budget = self.budget.load(Ordering::Acquire);
            let written = buf.len().min(budget);
            self.budget.fetch_sub(written, Ordering::AcqRel);
            self.bytes.fetch_add(written, Ordering::Relaxed);
            written
        }

        fn flush_echo_before_input(&self) -> bool {
            true
        }
    }

    struct PanicWriter;
    impl TtyWrite for PanicWriter {
        fn write(&self, _buf: &[u8]) {
            panic!("canonical input drain must not synchronously write echo bytes");
        }
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
            echo: EchoQueue::new(MockWriter, Arc::new(PollSet::new())),
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
    fn canonical_echo_is_batched_after_input_progress() {
        let (buf_tx, rx) = ReadBuf::default().split();
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let mut reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(b"hello\n".to_vec()),
            echo: EchoQueue::new(
                CountingWriter {
                    calls: calls.clone(),
                    bytes: bytes.clone(),
                },
                Arc::new(PollSet::new()),
            ),
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };

        assert!(reader.drain_source_into_line_buffer());
        assert_eq!(rx.occupied_len(), b"hello\n".len());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(bytes.load(Ordering::Relaxed), 0);

        reader.echo.drain_available();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(bytes.load(Ordering::Relaxed), b"hello\r\n".len());
    }

    #[test]
    fn canonical_echo_can_be_flushed_before_input_is_returned() {
        let (buf_tx, rx) = ReadBuf::default().split();
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let mut reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(b"echo marker\n".to_vec()),
            echo: EchoQueue::new(
                OrderedEchoWriter {
                    calls: calls.clone(),
                    bytes: bytes.clone(),
                },
                Arc::new(PollSet::new()),
            ),
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };

        assert!(reader.drain_source_into_line_buffer());
        assert_eq!(rx.occupied_len(), b"echo marker\n".len());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(bytes.load(Ordering::Relaxed), b"echo marker\r\n".len());
    }

    #[test]
    fn canonical_small_echo_respects_sync_limit() {
        let (buf_tx, rx) = ReadBuf::default().split();
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let mut reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(b"echo marker\n".to_vec()),
            echo: EchoQueue::new(
                LimitedEchoWriter {
                    calls: calls.clone(),
                    bytes: bytes.clone(),
                    limit: 64,
                },
                Arc::new(PollSet::new()),
            ),
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };

        assert!(reader.drain_source_into_line_buffer());
        assert_eq!(rx.occupied_len(), b"echo marker\n".len());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(bytes.load(Ordering::Relaxed), b"echo marker\r\n".len());
    }

    #[test]
    fn canonical_large_echo_exceeding_sync_limit_is_queued() {
        let (buf_tx, rx) = ReadBuf::default().split();
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let mut input = vec![b'a'; 128];
        input.push(b'\n');
        let mut reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(input),
            echo: EchoQueue::new(
                LimitedEchoWriter {
                    calls: calls.clone(),
                    bytes: bytes.clone(),
                    limit: 64,
                },
                Arc::new(PollSet::new()),
            ),
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };

        assert!(reader.drain_source_into_line_buffer());
        assert_eq!(rx.occupied_len(), 129);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(bytes.load(Ordering::Relaxed), 0);

        reader.echo.drain_available();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(bytes.load(Ordering::Relaxed), 130);
    }

    #[test]
    fn canonical_input_progress_does_not_wait_for_echo_writer() {
        let (buf_tx, rx) = ReadBuf::default().split();
        let mut reader = InputReader {
            terminal: Arc::new(Terminal::default()),
            reader: MockReader::new(b"burst\n".to_vec()),
            echo: EchoQueue::new(PanicWriter, Arc::new(PollSet::new())),
            buf_tx,
            read_buf: [0; BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };

        assert!(reader.drain_source_into_line_buffer());
        assert_eq!(rx.occupied_len(), b"burst\n".len());
    }

    #[test]
    fn synchronous_echo_backpressure_queues_unsent_suffix() {
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        let budget = Arc::new(AtomicUsize::new(2));
        let echo = EchoQueue::new(
            BackpressuredWriter {
                calls: calls.clone(),
                bytes: bytes.clone(),
                budget: budget.clone(),
            },
            Arc::new(PollSet::new()),
        );

        echo.write_now(b"abcdef");

        assert_eq!(bytes.load(Ordering::Relaxed), 2);
        assert_eq!(echo.queue.lock().len(), 4);

        budget.store(4, Ordering::Release);
        assert!(echo.drain_available());
        assert_eq!(bytes.load(Ordering::Relaxed), 6);
        assert!(echo.queue.lock().is_empty());
        assert!(calls.load(Ordering::Relaxed) >= 2);
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

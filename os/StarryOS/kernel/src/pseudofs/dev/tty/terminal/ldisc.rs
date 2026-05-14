use alloc::{sync::Arc, task::Wake, vec::Vec};
use core::{
    future::poll_fn,
    marker::PhantomData,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    task::{Poll, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_task::future::block_on;
use axpoll::PollSet;
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

// Keep input line-processing buffer small (one line is rarely > 80 chars), but
// give the ring buffer itself 4096 bytes so that a PTY passive-read can drain
// the full slave_to_master ring (also 4096) in a single poll_read() call.
// Without this, a 500-byte shell output generates ⌈500/80⌉ = 7 epoll round-trips
// with Tokio's EPOLLET reactor; with 4096 the same burst needs only one.
const LINE_BUF_SIZE: usize = 128;
const BUF_SIZE: usize = 4096;

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
}
pub trait TtyWrite: Send + Sync + 'static {
    fn write(&self, buf: &[u8]);
}

pub fn write_output_bytes<W: TtyWrite + ?Sized>(writer: &W, term: &Termios2, buf: &[u8]) {
    if !term.has_oflag(OPOST) || !term.has_oflag(ONLCR) {
        writer.write(buf);
        return;
    }

    // Collect output with \n→\r\n translation into a single buffer so we
    // make exactly one writer.write() call instead of one per newline.
    // This prevents partial-frame writes to the UART that cause visible
    // line-by-line flicker on serial-connected terminal emulators.
    let extra = buf.iter().filter(|&&b| b == b'\n').count();
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
    read_buf: [u8; LINE_BUF_SIZE],
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
    read_buf: [u8; LINE_BUF_SIZE],
    buf_tx: CachingProd<ReadBuf>,
}
impl<R: TtyRead> SimpleReader<R> {
    pub fn poll(&mut self) {
        // Drain all available data from the underlying source into buf_rx.
        // Looping ensures that a burst larger than LINE_BUF_SIZE (the scratch
        // chunk) still reaches the ring buffer in a single poll() call as long
        // as there is room in buf_rx (BUF_SIZE = 4096).
        loop {
            if self.buf_tx.vacant_len() == 0 {
                break;
            }
            let read = self.reader.read(&mut self.read_buf);
            if read == 0 {
                break;
            }
            let pushed = self.buf_tx.push_slice(&self.read_buf[..read]);
            if pushed < read {
                break; // buf_rx full
            }
        }
    }
}

enum Processor<R> {
    InterruptDriven,
    Passive(SimpleReader<R>, Arc<PollSet>),
}

pub struct LineDiscipline<R, W> {
    terminal: Arc<Terminal>,
    buf_rx: CachingCons<ReadBuf>,
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
            input_ready.wake();
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
                    input_source.register(&waker);
                    pump_retry.register(&waker);

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
            read_buf: [0; LINE_BUF_SIZE],
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
                        read_buf: [0; LINE_BUF_SIZE],
                        buf_tx,
                    },
                    poll_rx,
                )
            }
        };
        Self {
            terminal,
            buf_rx,
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
        self.eof_ready.store(false, Ordering::Release);
        self.clear_line_buf.store(true, Ordering::Relaxed);
    }

    pub fn poll_read(&mut self) -> bool {
        if let Processor::Passive(reader, _) = &mut self.processor {
            reader.poll();
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
                self.input_ready.register(waker);
            }
            Processor::Passive(_, set) => {
                set.register(waker);
            }
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
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
        self.pump_retry.clone().wake();
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::sync::atomic::AtomicBool;

    use ringbuf::traits::{Observer, Split};

    use super::{InputReader, LINE_BUF_SIZE, ReadBuf, TtyRead, TtyWrite};
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
            read_buf: [0; LINE_BUF_SIZE],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
            eof_ready: Arc::new(AtomicBool::new(false)),
            clear_line_buf: Arc::new(AtomicBool::new(false)),
        };
        (reader, buf_rx)
    }

    /// Regression test: a canonical-mode input longer than LINE_BUF_SIZE characters
    /// (with no newline in the first chunk) must not stall drain_source_into_line_buffer.
    ///
    /// Before the fix, the function returned `sent > 0` which was false after the
    /// first LINE_BUF_SIZE bytes were consumed into line_buf (no newline yet), causing
    /// drive_input() to stop looping and the remaining input (including the newline)
    /// to be silently dropped.  The board CI symptom was shell commands being
    /// truncated to the first LINE_BUF_SIZE characters.
    #[test]
    fn canonical_long_line_drain_continues_past_buf_size() {
        // LINE_BUF_SIZE ordinary chars followed by '\n' — total LINE_BUF_SIZE+1 bytes.
        let mut data: Vec<u8> = (0..LINE_BUF_SIZE).map(|_| b'a').collect();
        data.push(b'\n');

        let (mut reader, mut rx) = make_reader(data);

        // First drain: reads the LINE_BUF_SIZE 'a' bytes into line_buf; no newline yet,
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
}

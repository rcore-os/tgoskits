use alloc::{sync::Arc, task::Wake, vec::Vec};
use core::{
    future::poll_fn,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_task::future::{block_on, poll_io};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::general::{
    ECHOCTL, ECHOK, ICRNL, IGNCR, ISIG, VEOF, VERASE, VKILL, VMIN, VTIME,
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
    /// Process inputs only on call to `read`
    ///
    /// This is the fallback strategy and is rather limited. For instance, you
    /// can't interrupt a running program by Ctrl+C unless it's not blocked on a
    /// `read` call to the terminal, since the signal is emitted only when
    /// inputs are being processed.
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

struct InputReader<R, W> {
    terminal: Arc<Terminal>,

    reader: R,
    writer: W,

    buf_tx: CachingProd<ReadBuf>,
    read_buf: [u8; BUF_SIZE],
    read_range: Range<usize>,

    line_buf: Vec<u8>,
    line_read: Option<usize>,
    clear_line_buf: Arc<AtomicBool>,
}
impl<R: TtyRead, W: TtyWrite> InputReader<R, W> {
    pub fn drain_source_into_line_buffer(&mut self) -> bool {
        if self.clear_line_buf.swap(false, Ordering::Relaxed) {
            self.line_buf.clear();
        }
        if self.read_range.is_empty() {
            let read = self.reader.read(&mut self.read_buf);
            self.read_range = 0..read;
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

            if ch == b'\r' {
                if term.has_iflag(IGNCR) {
                    continue;
                }
                if term.has_iflag(ICRNL) {
                    ch = b'\n';
                }
            }

            self.check_send_signal(&term, ch);

            if term.echo() {
                self.output_char(&term, ch);
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

            if term.is_eol(ch) || ch == term.special_char(VEOF) {
                if ch != term.special_char(VEOF) {
                    self.line_buf.push(ch);
                }
                if !self.line_buf.is_empty() {
                    self.line_read = Some(0);
                }
                continue;
            }

            if ch == b' ' || ch.is_ascii_graphic() {
                self.line_buf.push(ch);
                continue;
            }
        }

        sent > 0
    }

    fn check_send_signal(&self, term: &Termios2, ch: u8) {
        if !term.canonical() || !term.has_lflag(ISIG) {
            return;
        }
        if let Some(signo) = term.signo_for(ch)
            && let Some(pg) = self.terminal.job_control.foreground()
        {
            let sig = SignalInfo::new_kernel(signo);
            if let Err(err) = send_signal_to_process_group(pg.pgid(), Some(sig)) {
                warn!("Failed to send signal: {err:?}");
            }
        }
    }

    fn output_char(&self, term: &Termios2, ch: u8) {
        match ch {
            b'\n' => self.writer.write(b"\n"),
            b'\r' => self.writer.write(b"\r\n"),
            ch if ch == term.special_char(VERASE) => self.writer.write(b"\x08 \x08"),
            ch if ch == b' ' || ch.is_ascii_graphic() => self.writer.write(&[ch]),
            ch if ch.is_ascii_control() && term.has_lflag(ECHOCTL) => {
                self.writer.write(&[b'^', (ch + 0x40)]);
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
    buf_tx: CachingProd<ReadBuf>,
}
impl<R: TtyRead> SimpleReader<R> {
    pub fn poll(&mut self) {
        let read = self.reader.read(&mut self.read_buf);
        for ch in &self.read_buf[..read] {
            if *ch == b'\n' {
                let _ = self.buf_tx.try_push(b'\r');
            }
            let _ = self.buf_tx.try_push(*ch);
        }
    }
}

enum Processor<R, W> {
    Manual(InputReader<R, W>),
    InterruptDriven,
    Passive(SimpleReader<R>, Arc<PollSet>),
}

pub struct LineDiscipline<R, W> {
    terminal: Arc<Terminal>,
    buf_rx: CachingCons<ReadBuf>,
    input_ready: Arc<PollSet>,
    pump_retry: Arc<PollSet>,
    clear_line_buf: Arc<AtomicBool>,
    processor: Processor<R, W>,
}

struct WaitPollable<'a>(Option<&'a PollSet>);
impl Pollable for WaitPollable<'_> {
    fn poll(&self) -> IoEvents {
        unreachable!()
    }

    fn register(&self, context: &mut Context<'_>, _events: IoEvents) {
        if let Some(set) = self.0 {
            set.register(context.waker());
        } else {
            context.waker().wake_by_ref();
        }
    }
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

    pub fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Self {
        let (buf_tx, buf_rx) = ReadBuf::default().split();

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
            clear_line_buf: clear_line_buf.clone(),
        };

        let input_ready = Arc::new(PollSet::new());
        let pump_retry = Arc::new(PollSet::new());
        let processor = match config.process_mode {
            ProcessMode::Manual => Processor::Manual(reader),
            ProcessMode::InterruptDriven(input_source) => {
                Self::spawn_interrupt_driven_reader(
                    reader,
                    input_source,
                    input_ready.clone(),
                    pump_retry.clone(),
                );
                Processor::InterruptDriven
            }
            ProcessMode::Passive(poll_rx) => {
                let InputReader { reader, buf_tx, .. } = reader;
                Processor::Passive(
                    SimpleReader {
                        reader,
                        read_buf: [0; BUF_SIZE],
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
            clear_line_buf,
            processor,
        }
    }

    pub fn drain_input(&mut self) {
        self.buf_rx.clear();
        self.clear_line_buf.store(true, Ordering::Relaxed);
    }

    pub fn poll_read(&mut self) -> bool {
        match &mut self.processor {
            Processor::Manual(reader) => {
                reader.drain_source_into_line_buffer();
            }
            Processor::Passive(reader, _) => reader.poll(),
            _ => {}
        }
        !self.buf_rx.is_empty()
    }

    pub fn register_rx_waker(&self, waker: &Waker) {
        match &self.processor {
            Processor::Manual(_) => {
                waker.wake_by_ref();
            }
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

        let mut total_read = 0;
        if let Processor::Manual(reader) = &mut self.processor {
            loop {
                reader.drain_source_into_line_buffer();
                total_read += self.buf_rx.pop_slice(&mut buf[total_read..]);
                if total_read >= vmin {
                    return Ok(total_read);
                }
                ax_task::yield_now();
            }
        }

        let input_ready = self.input_ready.clone();
        let pump_retry = self.pump_retry.clone();
        let buf_rx = &mut self.buf_rx;
        let pollable = WaitPollable(Some(input_ready.as_ref()));
        block_on(poll_io(&pollable, IoEvents::IN, false, || {
            total_read += buf_rx.pop_slice(&mut buf[total_read..]);
            pump_retry.as_ref().wake();
            (total_read >= vmin)
                .then_some(total_read)
                .ok_or(AxError::WouldBlock)
        }))
    }
}

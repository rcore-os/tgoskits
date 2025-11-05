use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    future::poll_fn,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use axerrno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use axtask::future::{Poller, block_on};
use linux_raw_sys::general::{
    ECHOCTL, ECHOK, ICRNL, IGNCR, ISIG, VEOF, VERASE, VKILL, VMIN, VTIME,
};
use ringbuf::{
    CachingCons, CachingProd,
    traits::{Consumer, Observer, Producer, Split},
};
use starry_core::task::send_signal_to_process_group;
use starry_signal::SignalInfo;

use crate::terminal::{Terminal, termios::Termios2};

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
    /// Spawns task for processing inputs, relying on external events to wake
    /// up.
    ///
    /// In this mode a dedicated task is spawned to handle inputs. When there's
    /// nothing to read the argument is invoked to register rx waker.
    External(Box<dyn Fn(Waker) + Send + Sync>),
    /// Do not process inputs.
    ///
    /// This is only used by the master side of pseudo tty. The argument is the
    /// [`PollSet`] for incoming data.
    None(Arc<PollSet>),
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
    pub fn poll(&mut self) -> bool {
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

            if ch.is_ascii_graphic() {
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
    External(Arc<PollSet>),
    None(SimpleReader<R>, Arc<PollSet>),
}

pub struct LineDiscipline<R, W> {
    terminal: Arc<Terminal>,
    buf_rx: CachingCons<ReadBuf>,
    poll_tx: Arc<PollSet>,
    clear_line_buf: Arc<AtomicBool>,
    processor: Processor<R, W>,
}

struct WaitPollable<'a>(Option<&'a Arc<PollSet>>);
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

impl<R: TtyRead, W: TtyWrite> LineDiscipline<R, W> {
    pub fn new(terminal: Arc<Terminal>, config: TtyConfig<R, W>) -> Self {
        let (buf_tx, buf_rx) = ReadBuf::default().split();

        let clear_line_buf = Arc::new(AtomicBool::new(false));
        let mut reader = InputReader {
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

        let poll_tx = Arc::new(PollSet::new());
        let processor = match config.process_mode {
            ProcessMode::Manual => Processor::Manual(reader),
            ProcessMode::External(register) => {
                let poll_rx = Arc::new(PollSet::new());
                axtask::spawn(
                    {
                        let poll_rx = poll_rx.clone();
                        let poll_tx = poll_tx.clone();
                        move || {
                            block_on(poll_fn(|cx| {
                                while reader.poll() {
                                    poll_rx.wake();
                                }
                                poll_tx.register(cx.waker());
                                register(cx.waker().clone());
                                while reader.poll() {
                                    poll_rx.wake();
                                }
                                Poll::Pending
                            }))
                        }
                    },
                    "tty-reader".into(),
                );
                Processor::External(poll_rx)
            }
            ProcessMode::None(poll_rx) => {
                // Destruct the reader here
                Processor::None(
                    SimpleReader {
                        reader: reader.reader,
                        read_buf: [0; BUF_SIZE],
                        buf_tx: reader.buf_tx,
                    },
                    poll_rx,
                )
            }
        };
        Self {
            terminal,
            buf_rx,
            poll_tx,
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
                reader.poll();
            }
            Processor::None(reader, _) => reader.poll(),
            _ => {}
        }
        !self.buf_rx.is_empty()
    }

    pub fn register_rx_waker(&self, waker: &Waker) {
        match &self.processor {
            Processor::Manual(_) => {
                waker.wake_by_ref();
            }
            Processor::External(set) | Processor::None(_, set) => {
                set.register(waker);
            }
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if matches!(self.processor, Processor::None(_, _)) {
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

        if buf.len() < vmin as usize {
            return Err(AxError::WouldBlock);
        }

        let mut total_read = 0;
        let set = match &self.processor {
            Processor::Manual(_) => None,
            Processor::External(set) => Some(set),
            _ => unreachable!(),
        };
        let pollable = WaitPollable(set);
        Poller::new(&pollable, IoEvents::IN).poll(|| {
            total_read += self.buf_rx.pop_slice(&mut buf[total_read..]);
            self.poll_tx.wake();
            (total_read >= vmin)
                .then_some(total_read)
                .ok_or(AxError::WouldBlock)
        })
    }
}

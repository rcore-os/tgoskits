use alloc::{sync::Arc, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use axerrno::{LinuxError, LinuxResult};
use axhal::irq::register_irq_waker;
use axio::{IoEvents, PollSet, Pollable};
use axtask::future::{Poller, block_on};
use kspin::SpinNoPreempt;
use linux_raw_sys::general::{
    ECHOCTL, ECHOK, ICRNL, IGNCR, ISIG, VEOF, VERASE, VKILL, VMIN, VTIME,
};
use ringbuf::{
    CachingCons, CachingProd,
    traits::{Consumer, Observer, Producer, Split},
};
use starry_core::task::send_signal_to_process_group;
use starry_signal::SignalInfo;

use crate::terminal::{job::JobControl, termios::Termios2};

const BUF_SIZE: usize = 80;

type ReadBuf = Arc<ringbuf::StaticRb<u8, BUF_SIZE>>;

struct InputReader {
    termios: Arc<SpinNoPreempt<Arc<Termios2>>>,
    job_control: Arc<JobControl>,

    buf_tx: CachingProd<ReadBuf>,
    read_buf: [u8; BUF_SIZE],

    line_buf: Vec<u8>,
    clear_line_buf: Arc<AtomicBool>,
}
impl InputReader {
    pub fn poll(&mut self) -> bool {
        if self.clear_line_buf.swap(false, Ordering::Relaxed) {
            self.line_buf.clear();
        }
        let max_read = self.buf_tx.vacant_len().min(BUF_SIZE);
        let read = axhal::console::read_bytes(&mut self.read_buf[..max_read]);
        let term = self.termios.lock().clone();
        for mut ch in self.read_buf[..read].iter().copied() {
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
                let len = self.buf_tx.push_slice(&self.line_buf);
                assert_eq!(len, self.line_buf.len());
                self.line_buf.clear();
                continue;
            }

            if ch.is_ascii_graphic() {
                self.line_buf.push(ch);
                continue;
            }
        }

        !self.buf_tx.is_empty()
    }

    fn check_send_signal(&self, term: &Termios2, ch: u8) {
        if !term.canonical() || !term.has_lflag(ISIG) {
            return;
        }
        if let Some(signo) = term.signo_for(ch)
            && let Some(pg) = self.job_control.foreground()
        {
            let sig = SignalInfo::new_kernel(signo);
            if let Err(err) = send_signal_to_process_group(pg.pgid(), Some(sig)) {
                warn!("Failed to send signal: {err:?}");
            }
        }
    }

    fn output_char(&self, term: &Termios2, ch: u8) {
        use axhal::console::write_bytes;
        match ch {
            b'\n' => write_bytes(b"\n"),
            b'\r' => write_bytes(b"\r\n"),
            ch if ch == term.special_char(VERASE) => write_bytes(b"\x08 \x08"),
            ch if ch.is_ascii_graphic() => write_bytes(&[ch]),
            ch if ch.is_ascii_control() && term.has_lflag(ECHOCTL) => {
                write_bytes(&[b'^', (ch + 0x40)]);
            }
            other => {
                warn!("Ignored echo char: {:#x}", other);
            }
        }
    }
}

pub struct LineDiscipline {
    pub termios: Arc<SpinNoPreempt<Arc<Termios2>>>,
    buf_rx: CachingCons<ReadBuf>,
    poll_tx: Arc<PollSet>,
    clear_line_buf: Arc<AtomicBool>,
    /// The read part of the line discipline.
    ///
    /// This could either be:
    /// - `Ok(reader)`: The input is driven by polling
    /// - `Err(poll)`: The input is driven by external interrupt
    reader: Result<InputReader, Arc<PollSet>>,
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

impl LineDiscipline {
    pub fn new(job_control: Arc<JobControl>) -> Self {
        let termios = Arc::<SpinNoPreempt<Arc<Termios2>>>::default();
        let (buf_tx, buf_rx) = ReadBuf::default().split();
        let poll_tx = Arc::new(PollSet::new());

        let clear_line_buf = Arc::new(AtomicBool::new(false));
        let mut reader = InputReader {
            termios: termios.clone(),
            job_control: job_control.clone(),

            buf_tx,
            read_buf: [0; BUF_SIZE],

            line_buf: Vec::new(),
            clear_line_buf: clear_line_buf.clone(),
        };
        let reader = if let Some(irq) = axhal::console::enable_rx_interrupt() {
            let poll_rx = Arc::new(PollSet::new());
            axtask::spawn({
                let poll_rx = poll_rx.clone();
                let poll_tx = poll_tx.clone();
                move || {
                    block_on(poll_fn(|cx| {
                        while reader.poll() {
                            poll_rx.wake();
                        }
                        poll_tx.register(cx.waker());
                        register_irq_waker(irq as _, cx.waker());
                        while reader.poll() {
                            poll_rx.wake();
                        }
                        Poll::Pending
                    }))
                }
            });
            Err(poll_rx)
        } else {
            Ok(reader)
        };
        Self {
            termios,
            buf_rx,
            poll_tx,
            clear_line_buf,
            reader,
        }
    }

    pub fn drain_input(&mut self) {
        self.buf_rx.clear();
        self.clear_line_buf.store(true, Ordering::Relaxed);
    }

    pub fn poll_read(&mut self) -> bool {
        if let Ok(reader) = &mut self.reader {
            reader.poll();
        }
        !self.buf_rx.is_empty()
    }

    pub fn register_rx_waker(&self, waker: &Waker) {
        match &self.reader {
            Ok(_) => {
                waker.wake_by_ref();
            }
            Err(set) => {
                set.register(waker);
            }
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let term = self.termios.lock().clone();
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
            return Err(LinuxError::EAGAIN);
        }

        let mut total_read = 0;
        let pollable = WaitPollable(self.reader.as_ref().err());
        Poller::new(&pollable, IoEvents::IN).poll(|| {
            total_read += self.buf_rx.pop_slice(&mut buf[total_read..]);
            self.poll_tx.wake();
            (total_read >= vmin)
                .then_some(total_read)
                .ok_or(LinuxError::EAGAIN)
        })
    }
}

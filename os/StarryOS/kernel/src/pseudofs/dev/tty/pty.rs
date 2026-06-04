use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_kspin::SpinNoIrq;
use axpoll::PollSet;
use ringbuf::{
    Cons, HeapRb, Prod,
    traits::{Consumer, Producer},
};

use super::{
    Tty,
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
    },
};

const PTY_BUF_SIZE: usize = 4096;

pub type PtyDriver = Tty<PtyReader, PtyWriter>;

type Buffer = Arc<HeapRb<u8>>;

pub struct PtyReader {
    buffer: Cons<Buffer>,
    writer_closed: Arc<AtomicBool>,
}

impl PtyReader {
    pub fn new(buffer: Buffer, writer_closed: Arc<AtomicBool>) -> Self {
        Self {
            buffer: Cons::new(buffer),
            writer_closed,
        }
    }
}

impl TtyRead for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        self.buffer.pop_slice(buf)
    }

    fn closed(&self) -> bool {
        self.writer_closed.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct PtyWriter {
    buffer: Arc<SpinNoIrq<Prod<Buffer>>>,
    poll_rx: Arc<PollSet>,
    closed: Arc<AtomicBool>,
}

impl PtyWriter {
    pub fn new(buffer: Buffer, poll_rx: Arc<PollSet>, closed: Arc<AtomicBool>) -> Self {
        Self {
            buffer: Arc::new(SpinNoIrq::new(Prod::new(buffer))),
            poll_rx,
            closed,
        }
    }
}

impl TtyWrite for PtyWriter {
    fn write(&self, buf: &[u8]) {
        let read = self.buffer.lock().push_slice(buf);
        self.poll_rx.wake();
        if read < buf.len() {
            warn!("Discarding {} bytes written to pty", buf.len() - read);
        }
    }

    fn close(&self) {
        // Closing one side of a pty makes the peer's read side complete
        // immediately. Nix relies on this after killing its remote build hook:
        // the master must stop waiting for more hook output once the slave side
        // is gone.
        self.closed.store(true, Ordering::Release);
        self.poll_rx.wake();
    }
}

pub(crate) fn create_pty_pair() -> (Arc<PtyDriver>, Arc<PtyDriver>) {
    let master_to_slave = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let slave_to_master = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let poll_rx_slave = Arc::new(PollSet::new());
    let poll_rx_master = Arc::new(PollSet::new());
    let master_closed = Arc::new(AtomicBool::new(false));
    let slave_closed = Arc::new(AtomicBool::new(false));

    let terminal = Arc::new(Terminal::default());

    let master = Tty::new(
        terminal.clone(),
        TtyConfig {
            reader: PtyReader::new(slave_to_master.clone(), slave_closed.clone()),
            writer: PtyWriter::new(
                master_to_slave.clone(),
                poll_rx_slave.clone(),
                master_closed.clone(),
            ),
            process_mode: ProcessMode::Passive(poll_rx_master.clone()),
        },
    );

    let slave = Tty::new(
        terminal,
        TtyConfig {
            reader: PtyReader::new(master_to_slave, master_closed),
            writer: PtyWriter::new(slave_to_master, poll_rx_master, slave_closed),
            process_mode: ProcessMode::InterruptDriven(poll_rx_slave),
        },
    );

    (master, slave)
}

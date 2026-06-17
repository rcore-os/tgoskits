use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet};
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

pub struct PtyReader(Cons<Buffer>, Arc<AtomicBool>);

impl PtyReader {
    pub fn new(buffer: Buffer, writer_closed: Arc<AtomicBool>) -> Self {
        Self(Cons::new(buffer), writer_closed)
    }
}

impl TtyRead for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        self.0.pop_slice(buf)
    }

    fn closed(&self) -> bool {
        self.1.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct PtyWriter(Arc<SpinNoIrq<Prod<Buffer>>>, Arc<PollSet>, Arc<AtomicBool>);

impl PtyWriter {
    pub fn new(buffer: Buffer, poll_rx: Arc<PollSet>, writer_closed: Arc<AtomicBool>) -> Self {
        Self(
            Arc::new(SpinNoIrq::new(Prod::new(buffer))),
            poll_rx,
            writer_closed,
        )
    }
}

impl TtyWrite for PtyWriter {
    fn write(&self, buf: &[u8]) {
        let read = self.0.lock().push_slice(buf);
        // PTY bytes are committed before waking the peer reader.
        unsafe { self.1.wake(IoEvents::IN) };
        if read < buf.len() {
            warn!("Discarding {} bytes written to pty", buf.len() - read);
        }
    }

    fn close(&self) {
        // Mark the writer as closed so the reader (master) side can see
        // POLLHUP and read EOF.  The peer's poll/read path drains any
        // already-buffered bytes first, then reports hangup on the next
        // non-blocking poll or read after the buffer is empty.
        self.2.store(true, Ordering::Release);
        unsafe { self.1.wake(IoEvents::IN) };
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

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

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
use crate::sync::IrqMutex;

const PTY_BUF_SIZE: usize = 4096;

pub type PtyDriver = Tty<PtyReader, PtyWriter>;

type Buffer = Arc<HeapRb<u8>>;

type SharedConsumer = Arc<IrqMutex<Cons<Buffer>>>;

pub struct PtyReader(SharedConsumer, Arc<AtomicBool>);

impl PtyReader {
    pub fn new(consumer: SharedConsumer, writer_closed: Arc<AtomicBool>) -> Self {
        Self(consumer, writer_closed)
    }
}

impl TtyRead for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        read_pty_buffer(&mut self.0.lock(), buf)
    }

    fn discard_input(&mut self) -> crate::StarryResult<()> {
        self.0.lock().clear();
        Ok(())
    }

    fn closed(&self) -> bool {
        self.1.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct PtyWriter(
    Arc<IrqMutex<Prod<Buffer>>>,
    SharedConsumer,
    Arc<PollSet>,
    Arc<AtomicBool>,
);

impl PtyWriter {
    pub fn new(
        buffer: Buffer,
        consumer: SharedConsumer,
        poll_rx: Arc<PollSet>,
        writer_closed: Arc<AtomicBool>,
    ) -> Self {
        Self(
            Arc::new(IrqMutex::new(Prod::new(buffer))),
            consumer,
            poll_rx,
            writer_closed,
        )
    }
}

impl TtyWrite for PtyWriter {
    fn write(&self, buf: &[u8]) {
        let read = self.try_write(buf);
        if read < buf.len() {
            warn!("Discarding {} bytes written to pty", buf.len() - read);
        }
    }

    fn try_write(&self, buf: &[u8]) -> usize {
        let read = write_pty_buffer(&mut self.0.lock(), buf);
        // PTY bytes are committed before waking the peer reader.
        unsafe { self.2.wake(IoEvents::IN) };
        read
    }

    fn discard_output(&self) -> crate::StarryResult<()> {
        let _producer = self.0.lock();
        self.1.lock().clear();
        Ok(())
    }

    fn close(&self) {
        // Mark this writer side as fully closed so the peer reader can report
        // POLLHUP / read EOF, and wake the peer reader's poll set so its
        // blocked poll()/read() observe the hangup. The peer drains any
        // already-buffered bytes first, then sees hangup on the next poll/read
        // once the buffer is empty.
        self.3.store(true, Ordering::Release);
        unsafe { self.2.wake(IoEvents::IN) };
    }
}

fn read_pty_buffer(consumer: &mut Cons<Buffer>, buf: &mut [u8]) -> usize {
    consumer.pop_slice(buf)
}

fn write_pty_buffer(producer: &mut Prod<Buffer>, buf: &[u8]) -> usize {
    producer.push_slice(buf)
}

pub(crate) fn create_pty_pair() -> (Arc<PtyDriver>, Arc<PtyDriver>) {
    let master_to_slave = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let slave_to_master = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let poll_rx_slave = Arc::new(PollSet::new());
    let poll_rx_master = Arc::new(PollSet::new());
    // Shared close-flags: each writer sets its own flag on last-fd close so the
    // peer reader can observe hangup (POLLHUP / EOF).
    let master_closed = Arc::new(AtomicBool::new(false));
    let slave_closed = Arc::new(AtomicBool::new(false));
    let master_to_slave_consumer = Arc::new(IrqMutex::new(Cons::new(master_to_slave.clone())));
    let slave_to_master_consumer = Arc::new(IrqMutex::new(Cons::new(slave_to_master.clone())));

    let terminal = Arc::new(Terminal::default());

    let master = Tty::new(
        terminal.clone(),
        TtyConfig {
            reader: PtyReader::new(slave_to_master_consumer.clone(), slave_closed.clone()),
            writer: PtyWriter::new(
                master_to_slave.clone(),
                master_to_slave_consumer.clone(),
                poll_rx_slave.clone(),
                master_closed.clone(),
            ),
            process_mode: ProcessMode::Passive(poll_rx_master.clone()),
        },
    );

    let slave = Tty::new(
        terminal,
        TtyConfig {
            reader: PtyReader::new(master_to_slave_consumer, master_closed),
            writer: PtyWriter::new(
                slave_to_master,
                slave_to_master_consumer,
                poll_rx_master,
                slave_closed,
            ),
            process_mode: ProcessMode::InterruptDriven {
                input: poll_rx_slave,
                output: None,
            },
        },
    );

    (master, slave)
}

#[cfg(all(test, not(axtest)))]
fn pty_preserves_mouse_escape_reports_for_test() -> bool {
    let buffer = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let mut producer = Prod::new(buffer.clone());
    let mut consumer = Cons::new(buffer);
    let report = b"\x1b[<0;1;1M";

    if write_pty_buffer(&mut producer, report) != report.len() {
        return false;
    }

    let mut buf = [0; 16];
    let read = read_pty_buffer(&mut consumer, &mut buf);
    &buf[..read] == report
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn pty_preserves_mouse_escape_reports() {
        assert!(super::pty_preserves_mouse_escape_reports_for_test());
    }
}

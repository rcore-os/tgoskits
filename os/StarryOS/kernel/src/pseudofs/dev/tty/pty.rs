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
        let read = self.try_write(buf);
        if read < buf.len() {
            warn!("Discarding {} bytes written to pty", buf.len() - read);
        }
    }

    fn try_write(&self, buf: &[u8]) -> usize {
        let read = self.0.lock().push_slice(buf);
        // PTY bytes are committed before waking the peer reader.
        unsafe { self.1.wake(IoEvents::IN) };
        read
    }

    fn close(&self) {
        // Mark this writer side as fully closed so the peer reader can report
        // POLLHUP / read EOF, and wake the peer reader's poll set so its
        // blocked poll()/read() observe the hangup. The peer drains any
        // already-buffered bytes first, then sees hangup on the next poll/read
        // once the buffer is empty.
        self.2.store(true, Ordering::Release);
        unsafe { self.1.wake(IoEvents::IN) };
    }
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
            process_mode: ProcessMode::InterruptDriven {
                input: poll_rx_slave,
                output: None,
            },
        },
    );

    (master, slave)
}

#[cfg(test)]
mod tests {
    use axpoll::{IoEvents, Pollable};

    use crate::pseudofs::DeviceOps;

    #[test]
    fn pty_preserves_mouse_escape_reports() {
        let (master, slave) = super::create_pty_pair();
        let report = b"\x1b[<0;1;1M";

        assert_eq!(slave.write_at(report, 0), Ok(report.len()));
        assert!(master.poll().contains(IoEvents::IN));

        let mut buf = [0; 16];
        let read = master.read_at(&mut buf, 0).unwrap();
        assert_eq!(&buf[..read], report);
    }
}

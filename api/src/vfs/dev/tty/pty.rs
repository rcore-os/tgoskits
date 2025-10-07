use alloc::{boxed::Box, sync::Arc};

use axpoll::PollSet;
use kspin::SpinNoPreempt;
use ringbuf::{
    Cons, HeapRb, Prod,
    traits::{Consumer, Producer},
};

use super::Tty;
use crate::terminal::{
    Terminal,
    ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
};

const PTY_BUF_SIZE: usize = 4096;

pub type PtyDriver = Tty<PtyReader, PtyWriter>;

type Buffer = Arc<HeapRb<u8>>;

pub struct PtyReader(Cons<Buffer>);

impl PtyReader {
    pub fn new(buffer: Buffer) -> Self {
        Self(Cons::new(buffer))
    }
}

impl TtyRead for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        self.0.pop_slice(buf)
    }
}

#[derive(Clone)]
pub struct PtyWriter(Arc<SpinNoPreempt<Prod<Buffer>>>, Arc<PollSet>);

impl PtyWriter {
    pub fn new(buffer: Buffer, poll_rx: Arc<PollSet>) -> Self {
        Self(Arc::new(SpinNoPreempt::new(Prod::new(buffer))), poll_rx)
    }
}

impl TtyWrite for PtyWriter {
    fn write(&self, buf: &[u8]) {
        let read = self.0.lock().push_slice(buf);
        self.1.wake();
        if read < buf.len() {
            warn!("Discarding {} bytes written to pty", buf.len() - read);
        }
    }
}

pub(crate) fn create_pty_pair() -> (Arc<PtyDriver>, Arc<PtyDriver>) {
    let master_to_slave = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let slave_to_master = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let poll_rx_slave = Arc::new(PollSet::new());
    let poll_rx_master = Arc::new(PollSet::new());

    let terminal = Arc::new(Terminal::default());

    let master = Tty::new(
        terminal.clone(),
        TtyConfig {
            reader: PtyReader::new(slave_to_master.clone()),
            writer: PtyWriter::new(master_to_slave.clone(), poll_rx_slave.clone()),
            process_mode: ProcessMode::None(poll_rx_master.clone()),
        },
    );

    let slave = Tty::new(
        terminal,
        TtyConfig {
            reader: PtyReader::new(master_to_slave),
            writer: PtyWriter::new(slave_to_master, poll_rx_master),
            process_mode: ProcessMode::External(Box::new(move |waker| {
                poll_rx_slave.register(&waker)
            })),
        },
    );

    (master, slave)
}

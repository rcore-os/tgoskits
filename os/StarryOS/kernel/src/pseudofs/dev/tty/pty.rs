use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};

use axpoll::IoEvents;
use axpoll_set::PollSet;
use ringbuf::{
    Cons, HeapRb, Prod,
    traits::{Consumer, Producer},
};

use super::{
    PtsInstance, Tty,
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

/// What the devpts instance must do once one end of a pty has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotAction {
    Keep,
    /// The master is gone: Linux devpts_pty_kill() drops the node while a slave
    /// that is still open keeps the index.
    Hide,
    /// Neither end can be reached any more, so the index goes back.
    Release,
}

/// How many descriptors each end of one pty has open, and the index they hold.
#[derive(Default)]
struct PtyLinkState {
    master_opens: usize,
    slave_opens: usize,
    master_closed: bool,
    /// The ptmx open that creates this pty has not reached `open()` yet. Linux
    /// installs the master in tty_init_dev() before devpts_pty_new() publishes
    /// the node, so a slave open in that window must not see "master gone".
    master_creating: bool,
    slot: Option<(Weak<PtsInstance>, u32)>,
}

impl PtyLinkState {
    fn creating() -> Self {
        Self {
            master_creating: true,
            ..Self::default()
        }
    }

    /// Linux pty_open() refuses a slave whose master is gone with `EIO`.
    fn open(&mut self, master: bool) -> crate::StarryResult<()> {
        if master {
            self.master_creating = false;
            self.master_opens += 1;
        } else {
            if self.master_closed || (self.master_opens == 0 && !self.master_creating) {
                return Err(crate::StarryError::Io);
            }
            self.slave_opens += 1;
        }
        Ok(())
    }

    /// The ptmx open failed on its way to installing the master, so the index
    /// its creation reserved goes back exactly as a close would return it.
    fn abandon_creation(&mut self) -> SlotAction {
        if !self.master_creating {
            return SlotAction::Keep;
        }
        self.master_creating = false;
        self.master_closed = true;
        if self.slave_opens > 0 {
            SlotAction::Hide
        } else {
            SlotAction::Release
        }
    }

    fn opens(&self, master: bool) -> usize {
        if master {
            self.master_opens
        } else {
            self.slave_opens
        }
    }

    fn close(&mut self, master: bool) -> SlotAction {
        if master {
            self.master_opens = self.master_opens.saturating_sub(1);
            if self.master_opens > 0 {
                return SlotAction::Keep;
            }
            self.master_closed = true;
        } else {
            self.slave_opens = self.slave_opens.saturating_sub(1);
        }
        if !self.master_closed {
            return SlotAction::Keep;
        }
        if self.slave_opens > 0 {
            SlotAction::Hide
        } else {
            SlotAction::Release
        }
    }
}

/// Both ends of one pty account for their opens and closes here, so the index
/// cannot be released while an open of either end is in flight.
pub(crate) struct PtyLink {
    state: IrqMutex<PtyLinkState>,
    /// Set while an end has no descriptor open; its peer reads that as
    /// EOF/POLLHUP. Only ever written under `state`, together with the count.
    master_closed: Arc<AtomicBool>,
    slave_closed: Arc<AtomicBool>,
}

impl Drop for PtyLink {
    /// Both writers hold the link, and the line discipline holds a copy of one
    /// of them, so this is the first point at which nothing can open the pty
    /// again: a creation whose master never opened gives its index back here.
    fn drop(&mut self) {
        let mut state = self.state.lock();
        let action = state.abandon_creation();
        let Some((instance, index)) = state.slot.clone() else {
            return;
        };
        let Some(instance) = instance.upgrade() else {
            return;
        };
        match action {
            SlotAction::Keep => {}
            SlotAction::Hide => instance.hide_slave(index),
            SlotAction::Release => {
                drop(state);
                instance.release_slave(index);
            }
        }
    }
}

impl PtyLink {
    fn new(master_closed: Arc<AtomicBool>, slave_closed: Arc<AtomicBool>) -> Self {
        Self {
            state: IrqMutex::new(PtyLinkState::creating()),
            master_closed,
            slave_closed,
        }
    }

    fn end_closed(&self, master: bool) -> &AtomicBool {
        if master {
            &self.master_closed
        } else {
            &self.slave_closed
        }
    }

    pub(crate) fn bind(&self, instance: &Arc<PtsInstance>, index: u32) {
        self.state.lock().slot = Some((Arc::downgrade(instance), index));
    }

    /// Linux pty_open() clears TTY_OTHER_CLOSED on the peer, so an end that
    /// opens again stops reading as hung up from the other side.
    fn opening(&self, master: bool) -> crate::StarryResult<()> {
        let mut state = self.state.lock();
        state.open(master)?;
        self.end_closed(master).store(false, Ordering::Release);
        Ok(())
    }

    fn closing(&self, master: bool) {
        let mut state = self.state.lock();
        let action = state.close(master);
        if state.opens(master) == 0 {
            self.end_closed(master).store(true, Ordering::Release);
        }
        drop(state);
        self.settle(action);
    }

    fn settle(&self, action: SlotAction) {
        if matches!(action, SlotAction::Keep) {
            return;
        }
        let mut state = self.state.lock();
        let Some((instance, index)) = state.slot.clone() else {
            return;
        };
        let Some(instance) = instance.upgrade() else {
            state.slot = None;
            return;
        };
        match action {
            SlotAction::Keep => {}
            SlotAction::Hide => instance.hide_slave(index),
            SlotAction::Release => {
                state.slot = None;
                drop(state);
                instance.release_slave(index);
            }
        }
    }
}

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
    Arc<PtyLink>,
    bool,
);

impl PtyWriter {
    pub fn new(
        buffer: Buffer,
        consumer: SharedConsumer,
        poll_rx: Arc<PollSet>,
        link: Arc<PtyLink>,
        master: bool,
    ) -> Self {
        Self(
            Arc::new(IrqMutex::new(Prod::new(buffer))),
            consumer,
            poll_rx,
            link,
            master,
        )
    }
}

impl TtyWrite for PtyWriter {
    fn opened(&self) -> crate::StarryResult<()> {
        self.3.opening(self.4)
    }

    fn closing(&self) {
        self.3.closing(self.4);
    }

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
        // `closing()` already published the hangup under the link lock; wake
        // the peer so a blocked poll()/read() observes it once the bytes
        // buffered before the close are drained.
        unsafe { self.2.wake(IoEvents::IN) };
    }
}

fn read_pty_buffer(consumer: &mut Cons<Buffer>, buf: &mut [u8]) -> usize {
    consumer.pop_slice(buf)
}

fn write_pty_buffer(producer: &mut Prod<Buffer>, buf: &[u8]) -> usize {
    producer.push_slice(buf)
}

pub(crate) fn create_pty_pair() -> (Arc<PtyDriver>, Arc<PtyDriver>, Arc<PtyLink>) {
    let master_to_slave = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let slave_to_master = Arc::new(HeapRb::new(PTY_BUF_SIZE));
    let poll_rx_slave = Arc::new(PollSet::new());
    let poll_rx_master = Arc::new(PollSet::new());
    // Each end's hangup flag is set by `PtyLink` when that end's last
    // descriptor closes; the peer reader observes it as POLLHUP / EOF.
    let master_closed = Arc::new(AtomicBool::new(false));
    let slave_closed = Arc::new(AtomicBool::new(false));
    let master_to_slave_consumer = Arc::new(IrqMutex::new(Cons::new(master_to_slave.clone())));
    let slave_to_master_consumer = Arc::new(IrqMutex::new(Cons::new(slave_to_master.clone())));

    let terminal = Arc::new(Terminal::default());
    let link = Arc::new(PtyLink::new(master_closed.clone(), slave_closed.clone()));

    let master = Tty::new(
        terminal.clone(),
        TtyConfig {
            reader: PtyReader::new(slave_to_master_consumer.clone(), slave_closed.clone()),
            writer: PtyWriter::new(
                master_to_slave.clone(),
                master_to_slave_consumer.clone(),
                poll_rx_slave.clone(),
                link.clone(),
                true,
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
                link.clone(),
                false,
            ),
            process_mode: ProcessMode::InterruptDriven {
                input: poll_rx_slave,
                output: None,
            },
        },
    );

    (master, slave, link)
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
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::{PtyLink, PtyLinkState, SlotAction};

    #[test]
    fn pty_preserves_mouse_escape_reports() {
        assert!(super::pty_preserves_mouse_escape_reports_for_test());
    }

    /// The orders that matter are the ones an open and the master's close can
    /// take against each other; the index may only go back when neither end
    /// can still reach the pty.
    #[test]
    fn a_slave_open_before_the_master_close_keeps_the_index() {
        let mut state = PtyLinkState::default();
        state.open(true).unwrap();
        state.open(false).unwrap();
        assert_eq!(state.close(true), SlotAction::Hide);
        assert_eq!(state.close(false), SlotAction::Release);
    }

    #[test]
    fn a_slave_opened_while_the_master_is_being_created_is_allowed() {
        let mut state = PtyLinkState::creating();
        state.open(false).unwrap();
        state.open(true).unwrap();
        assert_eq!(state.close(true), SlotAction::Hide);
        assert_eq!(state.close(false), SlotAction::Release);
    }

    #[test]
    fn a_creation_whose_master_never_opens_returns_the_index() {
        let mut state = PtyLinkState::creating();
        assert_eq!(state.abandon_creation(), SlotAction::Release);
        assert!(state.open(false).is_err());
    }

    #[test]
    fn an_abandoned_creation_keeps_the_index_for_a_slave_that_is_open() {
        let mut state = PtyLinkState::creating();
        state.open(false).unwrap();
        assert_eq!(state.abandon_creation(), SlotAction::Hide);
        assert_eq!(state.close(false), SlotAction::Release);
    }

    #[test]
    fn abandoning_after_the_master_opened_changes_nothing() {
        let mut state = PtyLinkState::creating();
        state.open(true).unwrap();
        assert_eq!(state.abandon_creation(), SlotAction::Keep);
        assert_eq!(state.close(true), SlotAction::Release);
    }

    fn link() -> PtyLink {
        PtyLink::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn a_slave_reopened_while_the_master_stays_open_is_no_longer_hung_up() {
        let link = link();
        link.opening(true).unwrap();
        link.opening(false).unwrap();
        link.closing(false);
        assert!(link.slave_closed.load(Ordering::Acquire));
        link.opening(false).unwrap();
        assert!(!link.slave_closed.load(Ordering::Acquire));
    }

    #[test]
    fn the_hangup_follows_the_last_descriptor_of_an_end() {
        let link = link();
        link.opening(true).unwrap();
        link.opening(false).unwrap();
        link.opening(false).unwrap();
        link.closing(false);
        assert!(!link.slave_closed.load(Ordering::Acquire));
        link.closing(false);
        assert!(link.slave_closed.load(Ordering::Acquire));
    }

    #[test]
    fn a_slave_open_after_the_master_close_is_refused() {
        let mut state = PtyLinkState::default();
        state.open(true).unwrap();
        assert_eq!(state.close(true), SlotAction::Release);
        assert!(state.open(false).is_err());
    }

    #[test]
    fn the_index_returns_only_after_the_last_open_of_each_end() {
        let mut state = PtyLinkState::default();
        state.open(true).unwrap();
        state.open(false).unwrap();
        state.open(false).unwrap();
        assert_eq!(state.close(false), SlotAction::Keep);
        assert_eq!(state.close(true), SlotAction::Hide);
        assert_eq!(state.close(false), SlotAction::Release);
    }

    #[test]
    fn a_master_that_outlives_its_slave_keeps_the_index() {
        let mut state = PtyLinkState::default();
        state.open(true).unwrap();
        state.open(false).unwrap();
        assert_eq!(state.close(false), SlotAction::Keep);
        assert_eq!(state.close(true), SlotAction::Release);
    }

    #[test]
    fn a_pty_whose_slave_never_opened_frees_the_index() {
        let mut state = PtyLinkState::default();
        state.open(true).unwrap();
        assert_eq!(state.close(true), SlotAction::Release);
    }
}

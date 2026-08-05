use core::task::Context;

use bitflags::bitflags;

bitflags! {
    /// Filesystem I/O readiness events.
    ///
    /// The bit layout intentionally matches Linux `poll(2)`/`epoll(7)` event
    /// bits so OS layers can convert to their native poll type without losing
    /// information.
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub struct FsIoEvents: u32 {
        /// Available for read.
        const IN = 0x0001;
        /// Urgent data for read.
        const PRI = 0x0002;
        /// Available for write.
        const OUT = 0x0004;
        /// Error condition.
        const ERR = 0x0008;
        /// Hang up.
        const HUP = 0x0010;
        /// Invalid request.
        const NVAL = 0x0020;
        /// Normal data can be read.
        const RDNORM = 0x0040;
        /// Priority band data can be read.
        const RDBAND = 0x0080;
        /// Normal data can be written.
        const WRNORM = 0x0100;
        /// Priority data can be written.
        const WRBAND = 0x0200;
        /// Message.
        const MSG = 0x0400;
        /// Remove.
        const REMOVE = 0x1000;
        /// Stream socket peer closed connection, or shut down writing half.
        const RDHUP = 0x2000;

        /// Events that are always polled even without specifying them.
        const ALWAYS_POLL = Self::ERR.bits() | Self::HUP.bits();
    }
}

/// Trait for filesystem nodes that can report I/O readiness.
pub trait FsPollable {
    /// Polls for filesystem I/O events.
    fn poll(&self) -> FsIoEvents;

    /// Registers wakers for filesystem I/O events.
    fn register(&self, context: &mut Context<'_>, events: FsIoEvents);
}

#[cfg(test)]
mod tests {
    use super::FsIoEvents;

    #[test]
    fn fs_events_keep_linux_poll_bits() {
        assert_eq!(FsIoEvents::IN.bits(), 0x0001);
        assert_eq!(FsIoEvents::OUT.bits(), 0x0004);
        assert_eq!(
            (FsIoEvents::ERR | FsIoEvents::HUP).bits(),
            FsIoEvents::ALWAYS_POLL.bits()
        );
    }
}

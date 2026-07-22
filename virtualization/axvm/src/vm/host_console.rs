//! Host-console ownership capabilities used by virtual UART adapters.

use core::{
    ops::ControlFlow,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::SpinNoIrq;
use axdevice::{ConsoleTxPolicy, DeviceManagerError, DeviceManagerResult};

static EXCLUSIVE_RX_CLAIMED: AtomicBool = AtomicBool::new(false);
static TX_OWNERSHIP: SpinNoIrq<HostConsoleTxOwnership> =
    SpinNoIrq::new(HostConsoleTxOwnership::new());

struct HostConsoleTxOwnership {
    shared_writers: usize,
    exclusive_writer: bool,
}

impl HostConsoleTxOwnership {
    const fn new() -> Self {
        Self {
            shared_writers: 0,
            exclusive_writer: false,
        }
    }
}

pub(crate) struct HostConsoleRxLease {
    input: SpinNoIrq<HostConsoleInputBuffer<32>>,
}

impl HostConsoleRxLease {
    pub(crate) fn claim() -> DeviceManagerResult<Self> {
        EXCLUSIVE_RX_CLAIMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| DeviceManagerError::ResourceConflict {
                operation: "claim host-console receive backend",
                detail: "another virtual console already owns exclusive host input".into(),
            })?;
        Ok(Self {
            input: SpinNoIrq::new(HostConsoleInputBuffer::new()),
        })
    }

    /// Presents the oldest buffered byte and consumes it only when requested.
    ///
    /// Returning [`ControlFlow::Continue`] consumes the byte; returning
    /// [`ControlFlow::Break`] keeps it for the next attempt.
    ///
    /// Keeping the byte until the emulated UART accepts it closes the race
    /// between a readiness check and the UART state transition itself.
    pub(crate) fn with_next_byte<E>(
        &self,
        process: impl FnOnce(u8) -> Result<ControlFlow<(), ()>, E>,
    ) -> Result<bool, E> {
        let mut input = self.input.lock();
        input.refill_from(ax_std::os::arceos::modules::ax_hal::console::read_bytes);
        let Some(byte) = input.front() else {
            return Ok(false);
        };
        if process(byte)?.is_continue() {
            let consumed = input.consume_front();
            debug_assert_eq!(consumed, Some(byte));
        }
        Ok(true)
    }
}

/// Bytes already removed from the host console but not yet accepted by a
/// guest UART.
///
/// Host console reads are destructive, while hardware UART FIFOs may apply
/// backpressure. Keeping this queue in the adapter prevents a polling batch
/// from turning normal host input into a synthetic guest FIFO overrun.
struct HostConsoleInputBuffer<const CAPACITY: usize> {
    bytes: [u8; CAPACITY],
    next: usize,
    end: usize,
}

impl<const CAPACITY: usize> HostConsoleInputBuffer<CAPACITY> {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            next: 0,
            end: 0,
        }
    }

    const fn has_pending(&self) -> bool {
        self.next != self.end
    }

    fn front(&self) -> Option<u8> {
        self.bytes
            .get(self.next)
            .copied()
            .filter(|_| self.has_pending())
    }

    fn consume_front(&mut self) -> Option<u8> {
        let byte = self.front()?;
        self.next += 1;
        Some(byte)
    }

    fn refill_from(&mut self, read: impl FnOnce(&mut [u8]) -> usize) {
        if self.has_pending() {
            return;
        }
        self.next = 0;
        self.end = read(&mut self.bytes).min(self.bytes.len());
    }
}

impl Drop for HostConsoleRxLease {
    fn drop(&mut self) {
        EXCLUSIVE_RX_CLAIMED.store(false, Ordering::Release);
    }
}

pub(crate) struct HostConsoleTxLease {
    policy: ConsoleTxPolicy,
}

impl HostConsoleTxLease {
    pub(crate) fn claim(policy: ConsoleTxPolicy) -> DeviceManagerResult<Option<Self>> {
        let mut ownership = TX_OWNERSHIP.lock();
        match policy {
            ConsoleTxPolicy::Disabled => return Ok(None),
            ConsoleTxPolicy::Shared if ownership.exclusive_writer => {
                return Err(tx_conflict("an exclusive writer already owns host output"));
            }
            ConsoleTxPolicy::Shared => {
                ownership.shared_writers =
                    ownership.shared_writers.checked_add(1).ok_or_else(|| {
                        DeviceManagerError::ResourceConflict {
                            operation: "claim host-console transmit backend",
                            detail: "shared host-console writer count overflowed".into(),
                        }
                    })?;
            }
            ConsoleTxPolicy::Exclusive
                if ownership.exclusive_writer || ownership.shared_writers != 0 =>
            {
                return Err(tx_conflict("host output already has an active writer"));
            }
            ConsoleTxPolicy::Exclusive => ownership.exclusive_writer = true,
        }
        drop(ownership);
        Ok(Some(Self { policy }))
    }

    pub(crate) fn write(&self, bytes: &[u8]) {
        let _ownership = TX_OWNERSHIP.lock();
        ax_std::os::arceos::modules::ax_hal::console::write_bytes(bytes);
    }
}

impl Drop for HostConsoleTxLease {
    fn drop(&mut self) {
        let mut ownership = TX_OWNERSHIP.lock();
        match self.policy {
            ConsoleTxPolicy::Shared => ownership.shared_writers -= 1,
            ConsoleTxPolicy::Exclusive => ownership.exclusive_writer = false,
            ConsoleTxPolicy::Disabled => {}
        }
    }
}

fn tx_conflict(detail: &'static str) -> DeviceManagerError {
    DeviceManagerError::ResourceConflict {
        operation: "claim host-console transmit backend",
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_transmit_ownership_conflicts_with_shared_ownership() {
        let shared = HostConsoleTxLease::claim(ConsoleTxPolicy::Shared)
            .unwrap()
            .unwrap();
        assert!(HostConsoleTxLease::claim(ConsoleTxPolicy::Exclusive).is_err());
        drop(shared);

        let exclusive = HostConsoleTxLease::claim(ConsoleTxPolicy::Exclusive)
            .unwrap()
            .unwrap();
        assert!(HostConsoleTxLease::claim(ConsoleTxPolicy::Shared).is_err());
        drop(exclusive);
    }

    #[test]
    fn buffered_input_retains_unaccepted_host_bytes() {
        let mut input = HostConsoleInputBuffer::<4>::new();
        input.refill_from(|buffer| {
            buffer.copy_from_slice(b"abcd");
            buffer.len()
        });

        assert_eq!(input.consume_front(), Some(b'a'));
        input.refill_from(|_| panic!("pending input must not be overwritten"));
        assert_eq!(input.consume_front(), Some(b'b'));
        assert_eq!(input.consume_front(), Some(b'c'));
        assert_eq!(input.consume_front(), Some(b'd'));
        assert!(!input.has_pending());
    }
}

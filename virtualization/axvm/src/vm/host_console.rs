//! Host-console ownership capabilities used by virtual UART adapters.

use core::sync::atomic::{AtomicBool, Ordering};

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

pub(crate) struct HostConsoleRxLease;

impl HostConsoleRxLease {
    pub(crate) fn claim() -> DeviceManagerResult<Self> {
        EXCLUSIVE_RX_CLAIMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| DeviceManagerError::ResourceConflict {
                operation: "claim host-console receive backend",
                detail: "another virtual console already owns exclusive host input".into(),
            })?;
        Ok(Self)
    }

    pub(crate) fn read(&self, bytes: &mut [u8]) -> usize {
        ax_std::os::arceos::modules::ax_hal::console::read_bytes(bytes)
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
}

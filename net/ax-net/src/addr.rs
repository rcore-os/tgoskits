//! Shared address and ephemeral-port helpers.

use ax_sync::RawSpinLock;
use smoltcp::wire::{IpAddress, Ipv4Address};

use crate::{NetError, NetResult};

const EPHEMERAL_PORT_START: u16 = 0xc000;
const EPHEMERAL_PORT_END: u16 = 0xffff;

/// Next ephemeral port candidate handed out by `allocate_ephemeral_port`.
static EPHEMERAL_PORT_CURSOR: RawSpinLock<u16> = RawSpinLock::new(EPHEMERAL_PORT_START);

/// Returns whether two wildcard/specific local addresses conflict on one port.
pub(crate) fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// Allocates an ephemeral port accepted by `check_available`.
pub(crate) fn allocate_ephemeral_port(check_available: impl Fn(u16) -> bool) -> NetResult<u16> {
    for _ in 0..=EPHEMERAL_PORT_END - EPHEMERAL_PORT_START {
        // Advance the cursor under the lock, but run the availability check
        // outside it: callbacks may take sleeping locks such as `udp_binds`.
        let port = {
            let mut curr = EPHEMERAL_PORT_CURSOR.lock();
            let port = *curr;
            *curr = if port == EPHEMERAL_PORT_END {
                EPHEMERAL_PORT_START
            } else {
                port + 1
            };
            port
        };
        if check_available(port) {
            return Ok(port);
        }
    }
    Err(NetError::AddrInUse)
}

/// Builds an IPv4 netmask from a CIDR prefix length.
pub(crate) fn mask_from_prefix(prefix_len: u8) -> Ipv4Address {
    let bits: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len.min(32) as u32)
    };
    Ipv4Address::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that drive the process-wide ephemeral port cursor, so
    /// one test cannot observe another test's in-flight allocation.
    static CURSOR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_cursor() -> std::sync::MutexGuard<'static, ()> {
        CURSOR_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    #[test]
    fn wildcard_addresses_conflict_with_specific_listeners() {
        let localhost = IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1));
        let peer = IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 2));

        assert!(listen_addrs_conflict(None, None));
        assert!(listen_addrs_conflict(None, Some(localhost)));
        assert!(listen_addrs_conflict(Some(localhost), Some(localhost)));
        assert!(!listen_addrs_conflict(Some(localhost), Some(peer)));
    }

    #[test]
    fn netmask_and_ephemeral_port_boundaries_hold() {
        let _cursor = lock_cursor();
        assert_eq!(mask_from_prefix(0), Ipv4Address::new(0, 0, 0, 0));
        assert_eq!(mask_from_prefix(8), Ipv4Address::new(255, 0, 0, 0));
        assert_eq!(mask_from_prefix(24), Ipv4Address::new(255, 255, 255, 0));
        assert_eq!(mask_from_prefix(33), Ipv4Address::new(255, 255, 255, 255));

        assert!(allocate_ephemeral_port(|port| port >= 0xc000).unwrap() >= 0xc000);
        assert_eq!(allocate_ephemeral_port(|_| false), Err(NetError::AddrInUse));
    }

    #[test]
    fn ephemeral_port_check_runs_without_allocator_lock() {
        let _cursor = lock_cursor();
        let port = allocate_ephemeral_port(|port| {
            assert!(
                EPHEMERAL_PORT_CURSOR.try_lock().is_some(),
                "the allocator cursor must be released before the availability check"
            );
            port >= EPHEMERAL_PORT_START
        })
        .unwrap();
        assert!((EPHEMERAL_PORT_START..=EPHEMERAL_PORT_END).contains(&port));
    }
}

//! Shared address and ephemeral-port helpers.

use ax_sync::Mutex;
use smoltcp::wire::{IpAddress, Ipv4Address};

use crate::{NetError, NetResult};

const EPHEMERAL_PORT_START: u16 = 0xc000;
const EPHEMERAL_PORT_END: u16 = 0xffff;

/// Returns whether two wildcard/specific local addresses conflict on one port.
pub(crate) fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// Allocates an ephemeral port accepted by `check_available`.
pub(crate) fn allocate_ephemeral_port(check_available: impl Fn(u16) -> bool) -> NetResult<u16> {
    static CURR: Mutex<u16> = Mutex::new(EPHEMERAL_PORT_START);

    let mut curr = CURR.lock();
    let mut tries = 0;
    while tries <= EPHEMERAL_PORT_END - EPHEMERAL_PORT_START {
        let port = *curr;
        if *curr == EPHEMERAL_PORT_END {
            *curr = EPHEMERAL_PORT_START;
        } else {
            *curr += 1;
        }
        if check_available(port) {
            return Ok(port);
        }
        tries += 1;
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
        assert_eq!(mask_from_prefix(0), Ipv4Address::new(0, 0, 0, 0));
        assert_eq!(mask_from_prefix(8), Ipv4Address::new(255, 0, 0, 0));
        assert_eq!(mask_from_prefix(24), Ipv4Address::new(255, 255, 255, 0));
        assert_eq!(mask_from_prefix(33), Ipv4Address::new(255, 255, 255, 255));

        assert!(allocate_ephemeral_port(|port| port >= 0xc000).unwrap() >= 0xc000);
        assert_eq!(allocate_ephemeral_port(|_| false), Err(NetError::AddrInUse));
    }
}

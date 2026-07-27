//! Shared address and ephemeral-port helpers.

use ax_errno::{AxResult, ax_bail};
use ax_sync::SpinMutex;
use smoltcp::wire::{IpAddress, Ipv4Address};

const EPHEMERAL_PORT_START: u16 = 0xc000;
const EPHEMERAL_PORT_END: u16 = 0xffff;

/// Returns whether two wildcard/specific local addresses conflict on one port.
pub(crate) fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// Allocates an ephemeral port accepted by `check_available`.
pub(crate) fn allocate_ephemeral_port(check_available: impl Fn(u16) -> bool) -> AxResult<u16> {
    static CURR: SpinMutex<u16> = SpinMutex::new(EPHEMERAL_PORT_START);

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
    ax_bail!(AddrInUse, "no available ports");
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

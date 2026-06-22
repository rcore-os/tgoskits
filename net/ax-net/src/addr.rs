//! Shared address and ephemeral-port helpers.

use ax_errno::{AxResult, ax_bail};
use ax_sync::Mutex;
use smoltcp::wire::IpAddress;

const EPHEMERAL_PORT_START: u16 = 0xc000;
const EPHEMERAL_PORT_END: u16 = 0xffff;

/// Returns whether two wildcard/specific local addresses conflict on one port.
pub(crate) fn listen_addrs_conflict(a: Option<IpAddress>, b: Option<IpAddress>) -> bool {
    a.is_none() || b.is_none() || a == b
}

/// Allocates an ephemeral port accepted by `check_available`.
pub(crate) fn allocate_ephemeral_port(check_available: impl Fn(u16) -> bool) -> AxResult<u16> {
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
    ax_bail!(AddrInUse, "no available ports");
}

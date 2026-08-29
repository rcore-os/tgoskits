mod addr;
mod cmsg;
mod io;
mod name;
mod opt;
mod socket;

#[cfg(test)]
pub(crate) use self::addr::net_addr_conversion_rules_hold_for_test;
#[cfg(test)]
pub(crate) use self::cmsg::cmsg_alignment_and_space_rules_hold_for_test;
#[cfg(test)]
pub(crate) use self::io::net_io_constants_hold_for_test;
#[cfg(test)]
pub(crate) use self::opt::net_opt_normalization_rules_hold_for_test;
#[cfg(test)]
pub(crate) use self::socket::net_socket_constants_hold_for_test;
pub use self::{cmsg::*, io::*, name::*, opt::*, socket::*};

/// [run6g] monotonic ns of the last unix-stream peer wake (read by do_poll
/// to measure wake-to-return latency).
pub fn unix_stream_last_peer_wake_ns() -> u64 {
    ax_net::unix::last_peer_wake_ns()
}

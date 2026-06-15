//! Orphan socket management for TCP connections.
//!
//! When a user closes a TCP socket (via Drop), the socket enters the orphan state:
//! - Unbound from the user-facing API (handle not accessible to new operations)
//! - smoltcp socket handle remains active in the protocol stack
//! - Connection teardown (FIN handshake, TIME_WAIT) continues in background
//! - Removed after smoltcp reaches Closed, or after TIME_WAIT/max linger expires
//!
//! This ensures RFC 793 compliance and prevents premature connection termination.

use alloc::vec::Vec;

use ax_sync::Mutex;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp,
    time::Instant,
};
use spin::LazyLock;

/// Orphaned TCP socket awaiting final cleanup.
struct OrphanSocket {
    handle: SocketHandle,
    orphaned_at: Instant,
}

/// Global orphan socket pool.
///
/// Accessed by:
/// - TcpSocket::drop() to add orphans
/// - net-poll worker to reap finished orphans
static ORPHAN_SOCKETS: LazyLock<Mutex<Vec<OrphanSocket>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Move a TCP socket to the orphan pool.
///
/// Called from TcpSocket::drop() after shutdown and endpoint cleanup.
pub(crate) fn add_orphan(handle: SocketHandle, timestamp: Instant) {
    ORPHAN_SOCKETS.lock().push(OrphanSocket {
        handle,
        orphaned_at: timestamp,
    });
}

/// Reap finished orphan sockets.
///
/// Called from net-poll worker on every poll cycle.
/// Removes sockets whose background TCP teardown no longer needs stack state.
pub(crate) fn reap_orphans(timestamp: Instant, sockets: &mut SocketSet<'_>) {
    const ORPHAN_MAX_LINGER: i64 = 60_000_000; // 60 seconds in microseconds
    const ORPHAN_MAX_SOCKETS: usize = 1024;

    let overflow = {
        let mut orphans = ORPHAN_SOCKETS.lock();
        let overflow = orphans.len().saturating_sub(ORPHAN_MAX_SOCKETS);
        if overflow == 0 {
            Vec::new()
        } else {
            orphans
                .drain(..overflow)
                .map(|orphan| orphan.handle)
                .collect()
        }
    };
    for handle in overflow {
        sockets.remove(handle);
        warn!("Reaped orphan socket {handle} because orphan pool is full");
    }

    ORPHAN_SOCKETS.lock().retain(|orphan| {
        let keep = {
            let socket = sockets.get_mut::<tcp::Socket>(orphan.handle);
            match socket.state() {
                tcp::State::Closed => false, // Fully closed, safe to remove
                tcp::State::TimeWait => {
                    // TIME_WAIT should expire naturally (smoltcp default: 10s)
                    // But force cleanup after max linger to prevent leaks
                    let elapsed = timestamp.total_micros() - orphan.orphaned_at.total_micros();
                    elapsed < ORPHAN_MAX_LINGER
                }
                tcp::State::LastAck | tcp::State::FinWait1 | tcp::State::FinWait2 => {
                    // Still tearing down, keep alive
                    true
                }
                tcp::State::Closing => true,
                _ => {
                    // Unexpected state for orphan (Listen/SynSent/SynReceived/Established)
                    // Force remove after max linger
                    let elapsed = timestamp.total_micros() - orphan.orphaned_at.total_micros();
                    if elapsed >= ORPHAN_MAX_LINGER {
                        warn!(
                            "Orphan socket {} in unexpected state {:?} after {}s, force removing",
                            orphan.handle,
                            socket.state(),
                            elapsed / 1_000_000
                        );
                        false
                    } else {
                        true
                    }
                }
            }
        };

        if !keep {
            sockets.remove(orphan.handle);
            debug!("Reaped orphan socket {}", orphan.handle);
        }
        keep
    });
}

/// Get current orphan socket count (for diagnostics).
#[allow(dead_code)]
pub(crate) fn orphan_count() -> usize {
    ORPHAN_SOCKETS.lock().len()
}

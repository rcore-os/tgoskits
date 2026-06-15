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

impl OrphanSocket {
    fn linger_micros(&self, timestamp: Instant) -> i64 {
        timestamp.total_micros() - self.orphaned_at.total_micros()
    }

    fn linger_expired(&self, timestamp: Instant) -> bool {
        self.linger_micros(timestamp) >= ORPHAN_MAX_LINGER
    }
}

/// Global orphan socket pool.
///
/// Accessed by:
/// - TcpSocket::drop() to add orphans
/// - net-poll worker to reap finished orphans
static ORPHAN_SOCKETS: LazyLock<Mutex<Vec<OrphanSocket>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

const ORPHAN_MAX_LINGER: i64 = 60_000_000; // 60 seconds in microseconds
const ORPHAN_MAX_SOCKETS: usize = 1024;

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
/// Removes orphan sockets after their background TCP teardown completes.
///
/// # Removal Conditions
///
/// - **Closed**: immediate removal (connection fully closed)
/// - **TimeWait**: removed after smoltcp timeout (~10s, max 60s)
/// - **FinWait1/FinWait2/LastAck/Closing**: kept until smoltcp transitions to Closed (max 60s)
/// - **Unexpected states** (Listen/SynSent/Established): force remove after 60s
///
/// # Overflow Protection
///
/// If the orphan pool exceeds 1024 sockets, closed or max-linger-expired entries
/// are removed first. Connections still inside the linger window are preserved
/// so normal FIN/TIME_WAIT teardown can complete.
pub(crate) fn reap_orphans(timestamp: Instant, sockets: &mut SocketSet<'_>) {
    ORPHAN_SOCKETS.lock().retain(|orphan| {
        let keep = {
            let socket = sockets.get_mut::<tcp::Socket>(orphan.handle);
            match socket.state() {
                tcp::State::Closed => false, // Fully closed, safe to remove
                tcp::State::TimeWait => {
                    // TIME_WAIT should expire naturally (smoltcp default: 10s)
                    // But force cleanup after max linger to prevent leaks
                    !orphan.linger_expired(timestamp)
                }
                tcp::State::LastAck | tcp::State::FinWait1 | tcp::State::FinWait2 => {
                    // Still tearing down, but keep a hard resource bound.
                    !orphan.linger_expired(timestamp)
                }
                tcp::State::Closing => !orphan.linger_expired(timestamp),
                _ => {
                    // Unexpected state for orphan (Listen/SynSent/SynReceived/Established)
                    // Force remove after max linger
                    let elapsed = orphan.linger_micros(timestamp);
                    if orphan.linger_expired(timestamp) {
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

    reap_overflow(timestamp, sockets, ORPHAN_MAX_SOCKETS);
}

fn reap_overflow(timestamp: Instant, sockets: &mut SocketSet<'_>, limit: usize) {
    let overflow = {
        let orphans = ORPHAN_SOCKETS.lock();
        orphans.len().saturating_sub(limit)
    };
    if overflow == 0 {
        return;
    }

    let mut removed = Vec::new();
    ORPHAN_SOCKETS.lock().retain(|orphan| {
        if removed.len() >= overflow {
            return true;
        }
        let socket = sockets.get_mut::<tcp::Socket>(orphan.handle);
        if socket.state() == tcp::State::Closed || orphan.linger_expired(timestamp) {
            removed.push(orphan.handle);
            false
        } else {
            true
        }
    });

    for handle in &removed {
        sockets.remove(*handle);
        warn!("Reaped orphan socket {handle} because orphan pool is full");
    }

    let remaining_overflow = ORPHAN_SOCKETS.lock().len().saturating_sub(limit);
    if remaining_overflow > 0 {
        warn!(
            "Orphan socket pool exceeds limit by {remaining_overflow}; keeping sockets that are \
             still tearing down"
        );
    }
}

/// Get current orphan socket count (for diagnostics).
#[allow(dead_code)]
pub(crate) fn orphan_count() -> usize {
    ORPHAN_SOCKETS.lock().len()
}

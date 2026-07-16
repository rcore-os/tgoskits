//! Orphan socket management for TCP connections.
//!
//! When a user closes a TCP socket, the userspace object is dropped immediately,
//! but the underlying smoltcp socket may still need to finish FIN exchange or
//! TIME-WAIT. This module keeps those sockets in a small orphan pool so the
//! dedicated net-poll worker can continue protocol teardown after the file
//! descriptor is gone.
//!
//! # Lifecycle
//!
//! - `TcpSocket::drop()` unregisters public bind/listen state and moves the
//!   smoltcp handle into the orphan pool.
//! - `reap_orphans()` runs from the poll path while `SocketSet` is already
//!   locked.
//! - Closed sockets are removed immediately; TIME-WAIT and FIN states are kept
//!   until smoltcp finishes or the maximum linger time expires.
//!
//! # Overflow Policy
//!
//! The pool has a hard capacity guard, but it does not blindly kill active
//! TIME-WAIT/FIN sockets just because the pool is full. Closed or expired
//! entries are reaped first; still-tearing-down entries are preserved so normal
//! TCP semantics win over aggressive cleanup.

use alloc::vec::Vec;

use ax_kspin::PreemptLazy as LazyLock;
use ax_sync::SpinMutex;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp,
    time::Instant,
};

/// Orphaned TCP socket awaiting final cleanup.
struct OrphanSocket {
    /// smoltcp socket handle kept alive after the public socket was dropped.
    handle: SocketHandle,
    /// Timestamp when the socket entered the orphan pool.
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

#[derive(Clone, Copy)]
enum ReapReason {
    /// smoltcp reached the Closed state.
    Closed,
    /// The orphan exceeded the maximum linger time.
    Expired,
}

/// Global orphan socket pool.
///
/// Accessed by:
/// - TcpSocket::drop() to add orphans
/// - net-poll worker to reap finished orphans
static ORPHAN_SOCKETS: LazyLock<SpinMutex<Vec<OrphanSocket>>> =
    LazyLock::new(|| SpinMutex::new(Vec::new()));

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
    let mut removed = Vec::new();
    let remaining_overflow = {
        let mut orphans = ORPHAN_SOCKETS.lock();
        orphans.retain(|orphan| {
            let socket = sockets.get_mut::<tcp::Socket>(orphan.handle);
            let state = socket.state();
            let reason = match state {
                tcp::State::Closed => Some(ReapReason::Closed),
                tcp::State::TimeWait => {
                    // TIME_WAIT should expire naturally (smoltcp default: 10s)
                    // But force cleanup after max linger to prevent leaks
                    orphan
                        .linger_expired(timestamp)
                        .then_some(ReapReason::Expired)
                }
                tcp::State::LastAck | tcp::State::FinWait1 | tcp::State::FinWait2 => {
                    // Still tearing down, but keep a hard resource bound.
                    orphan
                        .linger_expired(timestamp)
                        .then_some(ReapReason::Expired)
                }
                tcp::State::Closing => orphan
                    .linger_expired(timestamp)
                    .then_some(ReapReason::Expired),
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
                        Some(ReapReason::Expired)
                    } else {
                        None
                    }
                }
            };

            if let Some(reason) = reason {
                removed.push((orphan.handle, reason));
                false
            } else {
                true
            }
        });
        orphans.len().saturating_sub(ORPHAN_MAX_SOCKETS)
    };

    for (handle, reason) in removed {
        sockets.remove(handle);
        match reason {
            ReapReason::Closed => debug!("Reaped closed orphan socket {}", handle),
            ReapReason::Expired => warn!("Reaped expired orphan socket {}", handle),
        }
    }

    if remaining_overflow > 0 {
        warn!(
            "Orphan socket pool exceeds limit by {remaining_overflow}; keeping sockets that are \
             still tearing down"
        );
    }
}

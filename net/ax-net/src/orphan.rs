//! Orphan socket management for TCP connections.
//!
//! When a user closes a TCP socket, the userspace object is dropped immediately,
//! but the underlying smoltcp socket may still need to finish FIN exchange or
//! TIME-WAIT. This module keeps those sockets in a small orphan pool so the
//! unique protocol executor can continue protocol teardown after the file
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
//! Closed or expired entries are reaped first. Beyond that the pool is memory
//! bounded: TIME-WAIT entries have a dedicated bucket budget (oldest entries
//! are dropped past it, like Linux `tcp_max_tw_buckets`), and the hard socket
//! cap evicts oldest entries of any state so pooled buffers cannot exhaust the
//! heap under connect/close churn.

use alloc::vec::Vec;

use ax_lazyinit::LazyLock;
use ax_sync::Mutex;
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
/// - protocol executor to reap finished orphans
static ORPHAN_SOCKETS: LazyLock<Mutex<Vec<OrphanSocket>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

const ORPHAN_MAX_LINGER: i64 = 60_000_000; // 60 seconds in microseconds
const ORPHAN_MAX_SOCKETS: usize = 1024;

/// Pooled TIME-WAIT orphan budget, mirroring Linux `tcp_max_tw_buckets`.
///
/// Every pooled orphan keeps its full RX/TX buffers (512 KiB per TCP socket),
/// so churn-heavy workloads that close connections faster than TIME-WAIT
/// expires would otherwise pin unbounded memory and starve the heap. Dropping
/// the oldest TIME-WAIT entries beyond this budget trades rare duplicate
/// segment retransmission handling for a bounded footprint.
const ORPHAN_TIMEWAIT_BUCKETS: usize = 128;

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
/// Called from the protocol executor on every poll cycle.
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
/// TIME-WAIT orphans beyond [`ORPHAN_TIMEWAIT_BUCKETS`] are dropped oldest
/// first, mirroring Linux's bounded tw buckets; this keeps churn-heavy
/// workloads from pinning the full socket buffers of every lingering
/// connection. Past [`ORPHAN_MAX_SOCKETS`] total entries the oldest orphans of
/// any state are evicted so the pool cannot exhaust the heap.
pub(crate) fn reap_orphans(timestamp: Instant, sockets: &mut SocketSet<'_>) {
    let mut removed = Vec::new();
    {
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

        // Enforce the pool budgets. Entries are push-ordered, so scanning from
        // the front evicts the oldest orphans first. Past the hard cap any
        // state is evicted; below it only TIME-WAIT entries beyond their
        // dedicated budget are dropped, leaving FIN teardown untouched.
        let hard_overflow = orphans.len().saturating_sub(ORPHAN_MAX_SOCKETS);
        let mut eviction_budget = hard_overflow;
        let mut timewait_only = false;
        if eviction_budget == 0 {
            let timewait = orphans
                .iter()
                .filter(|orphan| {
                    sockets.get_mut::<tcp::Socket>(orphan.handle).state() == tcp::State::TimeWait
                })
                .count();
            eviction_budget = timewait.saturating_sub(ORPHAN_TIMEWAIT_BUCKETS);
            timewait_only = true;
        }
        let mut evicted = 0;
        let mut index = 0;
        while index < orphans.len() && eviction_budget > 0 {
            let state = sockets
                .get_mut::<tcp::Socket>(orphans[index].handle)
                .state();
            if timewait_only && state != tcp::State::TimeWait {
                index += 1;
                continue;
            }
            let orphan = orphans.remove(index);
            removed.push((orphan.handle, ReapReason::Expired));
            eviction_budget -= 1;
            evicted += 1;
        }
        if hard_overflow > 0 {
            warn!(
                "Orphan socket pool exceeded {ORPHAN_MAX_SOCKETS} sockets; evicted the oldest \
                 {evicted} entries"
            );
        }
    };

    for (handle, reason) in removed {
        sockets.remove(handle);
        match reason {
            ReapReason::Closed => debug!("Reaped closed orphan socket {}", handle),
            ReapReason::Expired => debug!("Reaped expired orphan socket {}", handle),
        }
    }
}

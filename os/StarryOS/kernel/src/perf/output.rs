//! Owned perf ring destination shared by task and IRQ producers.

use alloc::sync::{Arc, Weak};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

struct PerfRingState {
    ring_vaddr: usize,
    ring_len: usize,
    _anchor: Arc<dyn Any + Send + Sync>,
    writer_active: AtomicBool,
    lost_records: AtomicU64,
}

/// Kernel mapping geometry, lifetime, and producer serialization as one value.
#[derive(Clone)]
pub(crate) struct PerfRingOutput {
    state: Arc<PerfRingState>,
}

/// Non-owning reference retained by an event while the VMA or a redirect owns
/// the actual ring output.
#[derive(Clone)]
pub(crate) struct PerfRingWeak {
    state: Weak<PerfRingState>,
}

/// Context that owns a perf event's output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfOutputScope {
    /// Generation-bearing scheduler thread identity encoded as `u64`.
    Task(u64),
    /// Logical CPU id for a system-wide event.
    Cpu(usize),
}

/// Invalid `PERF_EVENT_IOC_SET_OUTPUT` relationship.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfOutputRedirectError {
    /// An event cannot redirect output to itself.
    SameEvent,
    /// Source and target do not share one task or CPU perf context.
    DifferentScope,
}

/// Own-ring and redirect state with one coherent selection point.
pub(crate) struct PerfOutputRoute {
    owned: Option<PerfRingWeak>,
    redirect: Option<PerfRingOutput>,
}

impl core::fmt::Debug for PerfOutputRoute {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfOutputRoute")
            .field("has_own_ring", &self.owned().is_some())
            .field("redirected", &self.redirect.is_some())
            .finish()
    }
}

/// Exclusive, bounded producer lease for one perf ring.
pub(crate) struct PerfRingWriteGuard<'a> {
    state: &'a PerfRingState,
}

impl Drop for PerfRingWriteGuard<'_> {
    fn drop(&mut self) {
        self.state.writer_active.store(false, Ordering::Release);
    }
}

impl PerfRingOutput {
    /// Builds an output snapshot from live ring geometry.
    pub(crate) fn new(
        ring_vaddr: usize,
        ring_len: usize,
        anchor: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            state: Arc::new(PerfRingState {
                ring_vaddr,
                ring_len,
                _anchor: anchor,
                writer_active: AtomicBool::new(false),
                lost_records: AtomicU64::new(0),
            }),
        }
    }

    /// Returns the kernel virtual address of the perf header page.
    pub(crate) fn ring_vaddr(&self) -> usize {
        self.state.ring_vaddr
    }

    /// Returns the complete mapping length, including the header page.
    pub(crate) fn ring_len(&self) -> usize {
        self.state.ring_len
    }

    /// Returns a non-owning event-side reference to this ring.
    pub(crate) fn downgrade(&self) -> PerfRingWeak {
        PerfRingWeak {
            state: Arc::downgrade(&self.state),
        }
    }

    /// Builds the opaque VMA retainer for this ring.
    ///
    /// Retaining the complete output keeps both the backing pages and the
    /// shared producer gate live for exactly as long as the mapping or a
    /// redirected event can publish records.
    pub(crate) fn mapping_anchor(&self) -> Arc<dyn Any + Send + Sync> {
        Arc::new(self.clone())
    }

    /// Tries to reserve the ring for one kernel producer.
    ///
    /// Hard-IRQ producers never wait: one failed CAS drops the record. Process
    /// producers use the same gate, so a redirected or inherited output cannot
    /// race an overflow running on another CPU.
    pub(crate) fn try_begin_write(&self) -> Option<PerfRingWriteGuard<'_>> {
        self.state
            .writer_active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| PerfRingWriteGuard { state: &self.state })
    }

    /// Accounts one record dropped because another producer owns the ring.
    pub(crate) fn record_contention_drop(&self) {
        self.state.lost_records.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the number of records dropped at the bounded writer gate.
    #[cfg(test)]
    pub(crate) fn contention_drops(&self) -> u64 {
        self.state.lost_records.load(Ordering::Relaxed)
    }
}

impl PerfRingWeak {
    /// Upgrades while either the user VMA or a redirect still owns the ring.
    pub(crate) fn upgrade(&self) -> Option<PerfRingOutput> {
        self.state.upgrade().map(|state| PerfRingOutput { state })
    }
}

impl PerfOutputRoute {
    /// Creates an event with no mmap ring and no redirect.
    pub(crate) const fn new() -> Self {
        Self {
            owned: None,
            redirect: None,
        }
    }

    /// Publishes the event's own mmap ring without retaining the VMA.
    pub(crate) fn publish_owned(&mut self, output: &PerfRingOutput) {
        self.owned = Some(output.downgrade());
    }

    /// Returns the event's own ring while a VMA or redirect still pins it.
    pub(crate) fn owned(&self) -> Option<PerfRingOutput> {
        self.owned.as_ref()?.upgrade()
    }

    /// Returns the effective output and whether it is redirected.
    pub(crate) fn effective(&self) -> Option<(PerfRingOutput, bool)> {
        self.redirect
            .clone()
            .map(|output| (output, true))
            .or_else(|| self.owned().map(|output| (output, false)))
    }

    /// Atomically replaces the redirect target.
    pub(crate) fn redirect(&mut self, output: PerfRingOutput) {
        self.redirect = Some(output);
    }

    /// Detaches a redirect so future writes use the event's own ring.
    pub(crate) fn detach(&mut self) {
        self.redirect = None;
    }

    /// Withdraws every output during final teardown.
    pub(crate) fn clear(&mut self) {
        self.redirect = None;
        self.owned = None;
    }
}

/// Validates the Linux same-context output relationship.
pub(crate) const fn validate_output_redirect(
    source_id: u64,
    target_id: u64,
    source_scope: PerfOutputScope,
    target_scope: PerfOutputScope,
) -> Result<(), PerfOutputRedirectError> {
    if source_id == target_id {
        return Err(PerfOutputRedirectError::SameEvent);
    }
    let same_scope = match (source_scope, target_scope) {
        (PerfOutputScope::Task(source), PerfOutputScope::Task(target)) => source == target,
        (PerfOutputScope::Cpu(source), PerfOutputScope::Cpu(target)) => source == target,
        _ => false,
    };
    if !same_scope {
        return Err(PerfOutputRedirectError::DifferentScope);
    }
    Ok(())
}

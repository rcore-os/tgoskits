//! Test-only allocation counting for the global allocator.
//!
//! An [`AllocAuditScope`] counts the heap allocations made by its CPU between
//! `enter` and `finish`. The count is exact, not statistical: the scope
//! disables local IRQs and preemption on its CPU, and each CPU counts into its
//! own cell, so other CPUs' allocations cannot contribute. Cells only grow;
//! a scope reports the difference from the value it saw on entry, so nested
//! scopes and scopes that overlap on other CPUs never disturb each other.
//! Allocations made while no scope is active cost one relaxed atomic load.

use core::{
    alloc::{GlobalAlloc, Layout},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use ax_sync::{PreemptGuard, PreemptIrqSaveGuard};

/// Number of scopes active on any CPU; the record hook counts while it is
/// non-zero, so a scope ending on one CPU cannot stop counting for another.
static ACTIVE_SCOPES: AtomicUsize = AtomicUsize::new(0);

/// Per-CPU allocation counter, only ever incremented.
struct AuditCell {
    allocations: AtomicU64,
}

impl AuditCell {
    const fn new() -> Self {
        Self {
            allocations: AtomicU64::new(0),
        }
    }
}

#[ax_percpu::def_percpu]
static AUDIT_CELL: AuditCell = AuditCell::new();

/// Returns this CPU's audit cell, or `None` before CPU areas are installed.
fn audit_cell() -> Option<&'static AuditCell> {
    // SAFETY: callers hold a preemption or IRQ guard for the callback, so the
    // current CPU cannot change while its area is resolved.
    let ptr = unsafe { ax_percpu::with_cpu_pin(|pin| AUDIT_CELL.current_ptr(pin)) }.ok()?;
    // SAFETY: the cell lives in this CPU's static area, which persists for
    // the lifetime of the system.
    Some(unsafe { ptr.as_ref() })
}

/// Counts this CPU's heap allocations until finished.
pub struct AllocAuditScope {
    start: u64,
    _irqs: PreemptIrqSaveGuard,
}

impl AllocAuditScope {
    /// Starts counting this CPU's allocations with local IRQs disabled.
    ///
    /// # Panics
    ///
    /// Panics if this CPU has not installed its runtime area, so tests call
    /// it only after boot.
    pub fn enter() -> Self {
        let irqs = PreemptIrqSaveGuard::new();
        // Publish the scope before sampling the start: every allocation this
        // CPU makes afterwards observes a non-zero count and is recorded.
        ACTIVE_SCOPES.fetch_add(1, Ordering::SeqCst);
        let cell = audit_cell().expect("allocation audit requires an installed CPU area");
        Self {
            start: cell.allocations.load(Ordering::Relaxed),
            _irqs: irqs,
        }
    }

    /// Stops counting and returns the number of allocations observed.
    pub fn finish(self) -> usize {
        let cell = audit_cell().expect("allocation audit requires an installed CPU area");
        let count = cell.allocations.load(Ordering::Relaxed) - self.start;
        ACTIVE_SCOPES.fetch_sub(1, Ordering::SeqCst);
        usize::try_from(count).expect("allocation count fits in a usize")
    }
}

/// Records one allocation against the current CPU's audit cell.
fn record_allocation() {
    if ACTIVE_SCOPES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let _preempt = PreemptGuard::new();
    if let Some(cell) = audit_cell() {
        cell.allocations.fetch_add(1, Ordering::Relaxed);
    }
}

/// The `#[global_allocator]` registered in alloc-audit builds: forwards every
/// operation unchanged to the selected backend and counts allocations.
pub struct AuditAllocator;

// SAFETY: every operation forwards the pointer and layout unchanged to the
// backend allocator; the observer only counts on the observing CPU.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: forwarded unchanged to the backend allocator.
        unsafe { GlobalAlloc::alloc(super::global_allocator(), layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: forwarded unchanged to the backend allocator.
        unsafe { GlobalAlloc::alloc_zeroed(super::global_allocator(), layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: forwarded unchanged to the backend allocator.
        unsafe { GlobalAlloc::realloc(super::global_allocator(), ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the backend allocator.
        unsafe { GlobalAlloc::dealloc(super::global_allocator(), ptr, layout) }
    }
}

use alloc::collections::btree_map::BTreeMap;
use core::{
    alloc::Layout,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::SpinNoIrq;
use axbacktrace::Backtrace;

pub(crate) static TRACKING_ENABLED: AtomicBool = AtomicBool::new(false);

#[ax_percpu::def_percpu]
pub(crate) static IN_GLOBAL_ALLOCATOR: bool = false;
// Re-entrancy note: `Backtrace::capture()` now uses `InlineFrames` (stack-allocated
// array, no heap) so it does NOT re-enter the allocator. The `IN_GLOBAL_ALLOCATOR`
// guard remains as a safety net for any future code paths that might allocate
// during backtrace capture.

/// Metadata for each allocation made by the global allocator.
#[derive(Debug)]
pub struct AllocationInfo {
    /// Layout of the allocation.
    pub layout: Layout,
    /// Backtrace at the time of allocation.
    pub backtrace: Backtrace,
    /// Generation at which the allocation was made.
    pub generation: u64,
}

pub(crate) struct GlobalState {
    // Ordered storage avoids randomized hash state in `no_std` builds and
    // keeps allocation reports deterministic.
    pub map: BTreeMap<usize, AllocationInfo>,
    pub generation: u64,
}

static STATE: SpinNoIrq<GlobalState> = SpinNoIrq::new(GlobalState {
    map: BTreeMap::new(),
    generation: 0,
});

/// Enables allocation tracking.
pub fn enable_tracking() {
    TRACKING_ENABLED.store(true, Ordering::SeqCst);
}

/// Disables allocation tracking.
pub fn disable_tracking() {
    TRACKING_ENABLED.store(false, Ordering::SeqCst);
}

/// Returns whether allocation tracking is enabled.
pub fn tracking_enabled() -> bool {
    TRACKING_ENABLED.load(Ordering::SeqCst)
}

pub(crate) fn with_state<R>(f: impl FnOnce(Option<&mut GlobalState>) -> R) -> R {
    let _guard = ax_kernel_guard::NoPreempt::new();
    // SAFETY: the guard prevents migration throughout all accesses below.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            if IN_GLOBAL_ALLOCATOR.read_current(pin) || !tracking_enabled() {
                return f(None);
            }

            IN_GLOBAL_ALLOCATOR.write_current(pin, true);
            let mut state = STATE.lock();
            let result = f(Some(&mut state));
            drop(state);
            IN_GLOBAL_ALLOCATOR.write_current(pin, false);
            result
        })
    }
    .expect("allocator tracking requires an installed CPU area")
}

/// Returns the current generation of the global allocator.
///
/// The generation is incremented every time a new allocation is made. It
/// can be utilized to track the changes in the allocation state over time.
///
/// See [`allocations_in`].
pub fn current_generation() -> u64 {
    STATE.lock().generation
}

/// Visits all allocations made by the global allocator within the given
/// generation range.
pub fn allocations_in(range: Range<u64>, visitor: impl FnMut(&AllocationInfo)) {
    with_state(|state| {
        state
            .unwrap()
            .map
            .values()
            .filter(move |info| range.contains(&info.generation))
            .for_each(visitor)
    });
}

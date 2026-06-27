//! Per-CPU hardware-PMU state for SMP + big.LITTLE `perf` (ARM PMUv3 only).
//!
//! All PMU system registers are banked per-PE, so each core owns its counter
//! count, cluster identity, and (Stage 2) counter allocator. This module
//! mirrors the already-per-CPU sampling [`super::sampling`] `REGISTRY`.
//!
//! [`ensure_core_inited`] runs the one-time clean-slate bring-up on the calling
//! core: it sets `PMCR_EL0.E`, clears all counter/IRQ enables and overflow
//! flags (so a freshly-entered secondary core cannot raise a spurious INTID 23
//! or carry a stale-enabled counter), and caches `PMCR.N` + the [`ClusterId`].
//! It is idempotent and cheap after the first call, and MUST run on the core it
//! initializes. The per-open `init_cpu()` call site is replaced by this, so the
//! clears happen exactly once per core and re-opens never disturb live counters.

use ax_cpu::pmu::{self, ClusterId};

/// Whether this core's PMU has had its one-time clean-slate bring-up.
#[ax_percpu::def_percpu]
static CORE_INITED: bool = false;

/// Cached programmable-counter count (`PMCR_EL0.N`) for this core.
#[ax_percpu::def_percpu]
static NUM_COUNTERS: usize = 0;

/// Cached cluster identity (`MIDR_EL1`) for this core.
#[ax_percpu::def_percpu]
static CLUSTER: ClusterId = ClusterId::Other(0);

/// Bring up the current core's PMU once, then return `(num_counters, cluster)`.
///
/// Idempotent and cheap after the first call. MUST run on the core it
/// initializes (PMU sysregs are per-PE banked).
pub fn ensure_core_inited() -> (usize, ClusterId) {
    if !CORE_INITED.read_current() {
        // PMCR.E (global enable) + P (reset programmable) + PMUSERENR (rdpmc).
        pmu::init_cpu();
        // Clean slate (≈ Linux armv8pmu_reset). Done ONCE per core, so re-opens
        // do not disable live counters of other events.
        pmu::counter::disable_all();
        pmu::overflow::disable_all_irq();
        pmu::overflow::clear_all();
        let num = pmu::probe().map(|i| i.num_counters).unwrap_or(0);
        let cluster = pmu::cluster_id();
        NUM_COUNTERS.write_current(num);
        // `ClusterId` is not a primitive int, so the `def_percpu` macro does not
        // generate `write_current`/`read_current` for it; use `with_current`
        // (which disables preemption for the access, nesting safely under the
        // IRQ-off scheduler hooks).
        CLUSTER.with_current(|c| *c = cluster);
        CORE_INITED.write_current(true);
    }
    (NUM_COUNTERS.read_current(), CLUSTER.with_current(|c| *c))
}

/// This core's programmable-counter count (after [`ensure_core_inited`]).
pub fn current_num_counters() -> usize {
    ensure_core_inited().0
}

/// Per-CPU programmable-counter allocator. `PMEVCNTRn_EL0` is banked per-PE, so
/// each core has its own pool of `num_counters` programmable counters plus the
/// dedicated cycle counter. A slot is reserved and released on the *same* core
/// within one scheduling slice (`perf_sched_in`/`perf_sched_out`), so the pool
/// stays coherent across task migration.
struct HwAlloc {
    /// Bitmask of allocated programmable counters (bit `n` ⇒ index `n` in use).
    used: u32,
    /// Whether the dedicated cycle counter is allocated.
    cycle_used: bool,
}

impl HwAlloc {
    const fn new() -> Self {
        HwAlloc {
            used: 0,
            cycle_used: false,
        }
    }
}

#[ax_percpu::def_percpu]
static ALLOC: HwAlloc = HwAlloc::new();

/// Allocate the lowest free programmable counter on the current core, or `None`
/// if all `num_counters` are in use.
///
/// Disables preemption + IRQs for the access so the per-CPU `ALLOC` is touched
/// on a stable core (mirrors [`super::sampling`]'s `REGISTRY` discipline).
pub fn alloc_programmable_counter() -> Option<usize> {
    let num = current_num_counters().min(32);
    let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
    // SAFETY: preemption + local IRQs are disabled by `_guard`, so we hold
    // exclusive access to this CPU's `ALLOC` for the critical section.
    let alloc = unsafe { ALLOC.current_ref_mut_raw() };
    for n in 0..num {
        if alloc.used & (1 << n) == 0 {
            alloc.used |= 1 << n;
            return Some(n);
        }
    }
    None
}

/// Release a programmable counter previously allocated on the current core.
pub fn free_programmable_counter(n: usize) {
    if n >= 32 {
        return;
    }
    let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
    // SAFETY: see [`alloc_programmable_counter`].
    let alloc = unsafe { ALLOC.current_ref_mut_raw() };
    alloc.used &= !(1 << n);
}

/// Allocate the dedicated cycle counter on the current core; `false` if taken.
pub fn alloc_cycle_counter() -> bool {
    let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
    // SAFETY: see [`alloc_programmable_counter`].
    let alloc = unsafe { ALLOC.current_ref_mut_raw() };
    if alloc.cycle_used {
        return false;
    }
    alloc.cycle_used = true;
    true
}

/// Release the dedicated cycle counter on the current core.
pub fn free_cycle_counter() {
    let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
    // SAFETY: see [`alloc_programmable_counter`].
    let alloc = unsafe { ALLOC.current_ref_mut_raw() };
    alloc.cycle_used = false;
}

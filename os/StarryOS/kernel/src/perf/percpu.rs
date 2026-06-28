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

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_cpu::pmu::{self, ClusterId};

/// Bitmask of online CPUs classified as Cortex-A55 (`Little`) by their real
/// `MIDR_EL1`, recorded by [`ensure_core_inited`] as each core comes up. Backs
/// the `armv8_cortex_a55` sysfs `cpus` mask.
static A55_CPUS: AtomicUsize = AtomicUsize::new(0);
/// Bitmask of online CPUs classified as Cortex-A76 (`Big`). Backs the
/// `armv8_cortex_a76` sysfs `cpus` mask.
static A76_CPUS: AtomicUsize = AtomicUsize::new(0);

/// Test-only override: when set, CPUs are classified by parity (even = `Little`,
/// odd = `Big`) instead of by `MIDR_EL1`, so a homogeneous machine (QEMU's
/// Cortex-A53) can exercise the big.LITTLE cluster-skip / dual-PMU logic. Off by
/// default; set via `/proc/sys/kernel/perf_test_force_clusters`.
static FORCE_CLUSTER_BY_PARITY: AtomicBool = AtomicBool::new(false);

/// Enable/disable the parity-based cluster override (test affordance).
pub fn set_force_clusters(on: bool) {
    FORCE_CLUSTER_BY_PARITY.store(on, Ordering::Release);
}

/// Whether the parity-based cluster override is currently enabled.
pub fn force_clusters_enabled() -> bool {
    FORCE_CLUSTER_BY_PARITY.load(Ordering::Acquire)
}

/// Classify a logical CPU into its [`ClusterId`].
///
/// Honors the parity test override first; otherwise reads the real-`MIDR`
/// classification recorded at that core's [`ensure_core_inited`]. A core not yet
/// brought up (and not under the override) reads as `Other(0)`.
pub fn cluster_of_cpu(cpu: usize) -> ClusterId {
    if FORCE_CLUSTER_BY_PARITY.load(Ordering::Acquire) {
        return if cpu % 2 == 0 {
            ClusterId::Little
        } else {
            ClusterId::Big
        };
    }
    let bit = 1usize.checked_shl(cpu as u32).unwrap_or(0);
    if A55_CPUS.load(Ordering::Acquire) & bit != 0 {
        ClusterId::Little
    } else if A76_CPUS.load(Ordering::Acquire) & bit != 0 {
        ClusterId::Big
    } else {
        ClusterId::Other(0)
    }
}

/// This core's effective cluster (honoring the test override).
pub fn current_cluster() -> ClusterId {
    cluster_of_cpu(ax_hal::percpu::this_cpu_id())
}

/// A set of clusters an event may run on. Generic/architectural events use
/// [`ClusterMask::ALL`]; an event opened against a cluster's sysfs PMU
/// (`armv8_cortex_a55` / `_a76`) is restricted to that cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterMask {
    /// Matches any cluster (generic events run everywhere, incl. `Other` cores).
    any: bool,
    little: bool,
    big: bool,
}

impl ClusterMask {
    /// All clusters — the default for generic / architectural events.
    pub const ALL: ClusterMask = ClusterMask {
        any: true,
        little: true,
        big: true,
    };
    /// Cortex-A55 (`Little`) only.
    pub const LITTLE_ONLY: ClusterMask = ClusterMask {
        any: false,
        little: true,
        big: false,
    };
    /// Cortex-A76 (`Big`) only.
    pub const BIG_ONLY: ClusterMask = ClusterMask {
        any: false,
        little: false,
        big: true,
    };

    /// Whether an event with this mask may run on a core of `cluster`.
    pub fn contains(self, cluster: ClusterId) -> bool {
        if self.any {
            return true;
        }
        match cluster {
            ClusterId::Little => self.little,
            ClusterId::Big => self.big,
            ClusterId::Other(_) => false,
        }
    }
}

/// Render the Linux-style `cpus` list (e.g. `0,2` or `4-7`) of the online CPUs in
/// `cluster`, for the per-cluster sysfs PMU's `cpus` file.
pub fn cluster_cpu_list(cluster: ClusterId) -> String {
    use core::fmt::Write;
    let n = ax_hal::cpu_num();
    let mut out = String::new();
    let mut first = true;
    for cpu in 0..n {
        if cluster_of_cpu(cpu) == cluster {
            if !first {
                out.push(',');
            }
            let _ = write!(out, "{cpu}");
            first = false;
        }
    }
    out.push('\n');
    out
}

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
        // Record this core's REAL-MIDR cluster in the global per-cluster CPU masks
        // (backs the dual sysfs PMUs' `cpus`). The parity test override is applied
        // on top in [`cluster_of_cpu`]; it does not touch these masks.
        let bit = 1usize
            .checked_shl(ax_hal::percpu::this_cpu_id() as u32)
            .unwrap_or(0);
        match cluster {
            ClusterId::Little => {
                A55_CPUS.fetch_or(bit, Ordering::AcqRel);
            }
            ClusterId::Big => {
                A76_CPUS.fetch_or(bit, Ordering::AcqRel);
            }
            ClusterId::Other(_) => {}
        }
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

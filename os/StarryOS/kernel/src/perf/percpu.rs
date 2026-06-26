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

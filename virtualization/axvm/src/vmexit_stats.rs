// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Per-CPU VM-exit reason counters.
//!
//! Pure statistics: every counter is `Relaxed` and participates in no
//! synchronization. Counters are laid out per physical CPU in cache-line
//! sized slots to avoid false sharing between the host vCPU tasks that run
//! on different pCPUs.

use std::sync::atomic::{AtomicU64, Ordering};

/// Number of tracked host physical CPUs, matching `axvm::percpu`.
pub const MAX_TRACKED_CPUS: usize = usize::BITS as usize;

/// VM-exit reason categories tracked per physical CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum ExitReason {
    /// A non-timer physical interrupt caused the exit.
    Irq,
    /// The host virtual-timer PPI caused the exit.
    Timer,
    /// MMIO read/write trapped for device emulation.
    Mmio,
    /// WFI or CPU_OFF standby wait.
    Wfi,
    /// HVC/SMC hypercall.
    Hvc,
    /// Trapped system-register access.
    SysReg,
    /// Trapped guest physical-timer (`CNTP_*`) access handled by `arm_vcpu`.
    PhysicalTimerSysReg,
    /// Trapped GIC CPU-interface register access.
    GicInterface,
    /// SGI (IPI) send.
    Sgi,
    /// PSCI CPU_ON secondary vCPU boot.
    CpuUp,
    /// PSCI SYSTEM_DOWN.
    SystemDown,
    /// The run slice completed without an event.
    Nothing,
    /// Any other exit not categorized above.
    Other,
}

impl ExitReason {
    /// Number of categories; must match the enum variant count.
    pub const COUNT: usize = 13;

    /// All categories in declaration order.
    pub const ALL: [ExitReason; Self::COUNT] = [
        ExitReason::Irq,
        ExitReason::Timer,
        ExitReason::Mmio,
        ExitReason::Wfi,
        ExitReason::Hvc,
        ExitReason::SysReg,
        ExitReason::PhysicalTimerSysReg,
        ExitReason::GicInterface,
        ExitReason::Sgi,
        ExitReason::CpuUp,
        ExitReason::SystemDown,
        ExitReason::Nothing,
        ExitReason::Other,
    ];

    /// Human-readable name for shell and log output.
    pub const fn name(self) -> &'static str {
        match self {
            ExitReason::Irq => "irq",
            ExitReason::Timer => "timer",
            ExitReason::Mmio => "mmio",
            ExitReason::Wfi => "wfi",
            ExitReason::Hvc => "hvc-smc",
            ExitReason::SysReg => "sysreg",
            ExitReason::PhysicalTimerSysReg => "cntp-sysreg",
            ExitReason::GicInterface => "gic-if",
            ExitReason::Sgi => "sgi",
            ExitReason::CpuUp => "cpu-up",
            ExitReason::SystemDown => "sys-down",
            ExitReason::Nothing => "nothing",
            ExitReason::Other => "other",
        }
    }

    #[cfg(any(target_arch = "aarch64", test))]
    fn index(self) -> usize {
        debug_assert!((self as usize) < Self::COUNT);
        self as usize
    }
}

/// Per-CPU counter slot. Sized and aligned to a cache line so that two host
/// CPUs never share a cache line when updating different slots.
#[repr(align(64))]
struct PerCpuExitCounters {
    counts: [AtomicU64; ExitReason::COUNT],
}

const fn new_per_cpu_counters() -> PerCpuExitCounters {
    PerCpuExitCounters {
        counts: [const { AtomicU64::new(0) }; ExitReason::COUNT],
    }
}

static EXIT_COUNTERS: [PerCpuExitCounters; MAX_TRACKED_CPUS] =
    [const { new_per_cpu_counters() }; MAX_TRACKED_CPUS];

/// Records one VM exit of `reason` on physical CPU `cpu_id`.
#[cfg(any(target_arch = "aarch64", test))]
pub fn note_exit(cpu_id: usize, reason: ExitReason) {
    let Some(slot) = EXIT_COUNTERS.get(cpu_id) else {
        warn!("VM-exit counter slot for CPU {cpu_id} is out of range");
        return;
    };
    slot.counts[reason.index()].fetch_add(1, Ordering::Relaxed);
}

/// Point-in-time per-CPU exit counter values.
#[derive(Clone, Debug)]
pub struct CpuExitCounts {
    /// Physical CPU the counters belong to.
    pub cpu_id: usize,
    /// Cumulative count per [`ExitReason`] category.
    pub counts: [u64; ExitReason::COUNT],
}

/// Snapshot every tracked CPU's cumulative counters.
pub fn vmexit_stats_snapshot() -> Vec<CpuExitCounts> {
    EXIT_COUNTERS
        .iter()
        .enumerate()
        .map(|(cpu_id, slot)| CpuExitCounts {
            cpu_id,
            counts: std::array::from_fn(|reason| slot.counts[reason].load(Ordering::Relaxed)),
        })
        .collect()
}

/// Clears the counters of a single physical CPU.
pub fn vmexit_stats_reset(cpu_id: usize) {
    let Some(slot) = EXIT_COUNTERS.get(cpu_id) else {
        warn!("VM-exit counter slot for CPU {cpu_id} is out of range");
        return;
    };
    for count in &slot.counts {
        count.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // The counters are process-global, so tests that mutate them must not run
    // concurrently with each other.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn counters(cpu_id: usize) -> Vec<u64> {
        vmexit_stats_snapshot()
            .into_iter()
            .find(|entry| entry.cpu_id == cpu_id)
            .expect("snapshot must include every tracked CPU")
            .counts
            .to_vec()
    }

    #[test]
    fn note_exit_increments_the_matching_category_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        vmexit_stats_reset(0);
        note_exit(0, ExitReason::Timer);
        note_exit(0, ExitReason::Timer);
        note_exit(0, ExitReason::Mmio);

        let counts = counters(0);
        assert_eq!(counts[ExitReason::Timer.index()], 2);
        assert_eq!(counts[ExitReason::Mmio.index()], 1);
        for (index, count) in counts.iter().enumerate() {
            if index != ExitReason::Timer.index() && index != ExitReason::Mmio.index() {
                assert_eq!(*count, 0, "unexpected count for category {index}");
            }
        }
    }

    #[test]
    fn counters_are_isolated_per_cpu() {
        let _guard = TEST_LOCK.lock().unwrap();
        vmexit_stats_reset(0);
        vmexit_stats_reset(1);
        note_exit(0, ExitReason::Irq);
        note_exit(1, ExitReason::Timer);

        assert_eq!(counters(0)[ExitReason::Irq.index()], 1);
        assert_eq!(counters(1)[ExitReason::Timer.index()], 1);
        assert_eq!(counters(0)[ExitReason::Timer.index()], 0);
        assert_eq!(counters(1)[ExitReason::Irq.index()], 0);
    }

    #[test]
    fn reset_clears_all_categories_of_one_cpu_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        vmexit_stats_reset(0);
        vmexit_stats_reset(1);
        note_exit(0, ExitReason::Hvc);
        note_exit(1, ExitReason::Hvc);

        vmexit_stats_reset(0);
        assert!(counters(0).iter().all(|count| *count == 0));
        assert_eq!(counters(1)[ExitReason::Hvc.index()], 1);
    }

    #[test]
    fn out_of_range_cpu_is_ignored_safely() {
        let _guard = TEST_LOCK.lock().unwrap();
        note_exit(MAX_TRACKED_CPUS, ExitReason::Irq);
        vmexit_stats_reset(MAX_TRACKED_CPUS);
        let snapshot = vmexit_stats_snapshot();
        assert!(snapshot.iter().all(|entry| entry.cpu_id < MAX_TRACKED_CPUS));
    }

    #[test]
    fn every_category_has_a_distinct_stable_name() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut names = std::collections::BTreeSet::new();
        for reason in ExitReason::ALL {
            assert!(names.insert(reason.name()), "duplicate name for {reason:?}");
            assert!(!reason.name().is_empty());
        }
    }
}

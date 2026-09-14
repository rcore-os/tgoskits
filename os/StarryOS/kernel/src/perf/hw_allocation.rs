//! Serialized fixed and CPU-local ARM PMUv3 counter reservations.
//!
//! Task-fixed cycle reservations exclude the native counter on every CPU.
//! System events and flexible slices reserve only their owner CPU. Both use
//! one allocation transaction so the two reservation classes cannot alias.

use super::hw_owner::Counter;
use crate::sync::IrqMutex;

struct HwAlloc {
    flexible: [u32; super::percpu::MAX_TRACKED_CPUS],
    cycle_used: bool,
    local_cycle_used: [bool; super::percpu::MAX_TRACKED_CPUS],
}

impl HwAlloc {
    const fn new() -> Self {
        Self {
            flexible: [0; super::percpu::MAX_TRACKED_CPUS],
            cycle_used: false,
            local_cycle_used: [false; super::percpu::MAX_TRACKED_CPUS],
        }
    }

    fn alloc_cycle(&mut self) -> Option<Counter> {
        if self.cycle_used || self.local_cycle_used.iter().any(|used| *used) {
            return None;
        }
        self.cycle_used = true;
        Some(Counter::Cycle)
    }

    fn alloc_flexible(&mut self, cpu: usize, num_counters: usize) -> Option<usize> {
        let local = self.flexible.get_mut(cpu)?;
        for n in 0..num_counters.min(32) {
            if *local & (1 << n) == 0 {
                *local |= 1 << n;
                return Some(n);
            }
        }
        None
    }

    fn alloc_system(
        &mut self,
        cpu: usize,
        prefer_cycle: bool,
        num_counters: usize,
    ) -> Option<Counter> {
        let local_cycle = self.local_cycle_used.get_mut(cpu)?;
        if prefer_cycle && !self.cycle_used && !*local_cycle {
            *local_cycle = true;
            return Some(Counter::Cycle);
        }
        self.alloc_flexible(cpu, num_counters)
            .map(Counter::Programmable)
    }
}

static ALLOC: IrqMutex<HwAlloc> = IrqMutex::new(HwAlloc::new());

pub(super) fn alloc_cycle_counter() -> Option<Counter> {
    ALLOC.lock().alloc_cycle()
}

/// Reserves only the native cycle counter on one CPU. If occupied, callers
/// construct a flexible logical event instead of reserving a fixed 32-bit slot.
pub(super) fn alloc_system_cycle(cpu: super::target::PerfCpuId) -> Option<Counter> {
    ALLOC.lock().alloc_system(cpu.as_usize(), true, 0)
}

pub(super) fn free_counter(counter: Counter) {
    assert_eq!(
        counter,
        Counter::Cycle,
        "only native cycles have task-fixed reservations"
    );
    let mut allocator = ALLOC.lock();
    assert!(allocator.cycle_used, "task cycle reservation must be owned");
    allocator.cycle_used = false;
}

/// Reserves a system event on its target PMU, independent of the opener CPU.
pub(super) fn alloc_system(
    cpu: super::target::PerfCpuId,
    event: u16,
    prefer_cycle: bool,
    num_counters: usize,
) -> crate::StarryResult<Counter> {
    let cpu = cpu.as_usize();
    if !super::percpu::cpu_info(cpu)
        .is_some_and(|info| crate::perf::event_map::event_supported_by(info, event))
    {
        return Err(crate::StarryError::Unsupported);
    }
    ALLOC
        .lock()
        .alloc_system(cpu, prefer_cycle, num_counters)
        .ok_or(crate::StarryError::ResourceBusy)
}

/// Releases a system reservation after its owner CPU has quiesced the slot.
pub(super) fn free_system(cpu: super::target::PerfCpuId, counter: Counter) {
    match counter {
        Counter::Cycle => {
            let mut allocator = ALLOC.lock();
            let used = &mut allocator.local_cycle_used[cpu.as_usize()];
            assert!(*used, "system cycle reservation must be owned");
            *used = false;
        }
        Counter::Programmable(slot) => free_flexible(cpu.as_usize(), slot),
    }
}

/// Reserves a CPU-local programmable slot against other local owners.
pub(super) fn alloc_flexible(cpu: usize, num_counters: usize) -> Option<usize> {
    ALLOC.lock().alloc_flexible(cpu, num_counters)
}

/// Releases one scheduler-owned CPU-local slot.
pub(super) fn free_flexible(cpu: usize, slot: usize) {
    let mut allocator = ALLOC.lock();
    let used = allocator.flexible.get_mut(cpu).expect("validated perf CPU");
    assert!(slot < 32 && *used & (1 << slot) != 0);
    *used &= !(1 << slot);
}

#[cfg(all(test, axtest))]
mod tests {
    #[axtest::axtest]
    fn task_and_system_cycle_reservations_are_mutually_exclusive() {
        let mut allocator = super::HwAlloc::new();
        assert_eq!(
            allocator.alloc_system(0, true, 0),
            Some(super::Counter::Cycle)
        );
        assert!(allocator.alloc_cycle().is_none());
        allocator.local_cycle_used[0] = false;
        assert_eq!(allocator.alloc_cycle(), Some(super::Counter::Cycle));
        assert!(allocator.alloc_system(0, true, 0).is_none());
        // The fixed cycle reservation must not reduce programmable capacity.
        assert_eq!(allocator.alloc_flexible(0, 1), Some(0));
        assert!(allocator.alloc_flexible(0, 1).is_none());
        assert_eq!(allocator.alloc_flexible(1, 1), Some(0));
    }
}

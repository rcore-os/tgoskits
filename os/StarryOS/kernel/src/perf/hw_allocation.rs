//! Serialized fixed and CPU-local ARM PMUv3 counter reservations.
//!
//! Task-fixed reservations exclude a slot on every CPU they can migrate to.
//! System events and flexible slices reserve only their owner CPU. Both use
//! one allocation transaction so the two reservation classes cannot alias.

use super::hw_owner::Counter;
use crate::sync::IrqMutex;

struct HwAlloc {
    used: u32,
    flexible: [u32; super::percpu::MAX_TRACKED_CPUS],
    cycle_used: bool,
    local_cycle_used: [bool; super::percpu::MAX_TRACKED_CPUS],
}

impl HwAlloc {
    const fn new() -> Self {
        Self {
            used: 0,
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

    fn alloc_counter(&mut self, num_counters: usize) -> Option<Counter> {
        let occupied = self.flexible.iter().fold(self.used, |used, cpu| used | cpu);
        for n in 0..num_counters.min(32) {
            if occupied & (1 << n) == 0 {
                self.used |= 1 << n;
                return Some(Counter::Programmable(n));
            }
        }
        None
    }

    fn alloc_flexible(&mut self, cpu: usize, num_counters: usize) -> Option<usize> {
        let local = self.flexible.get_mut(cpu)?;
        for n in 0..num_counters.min(32) {
            if (*local | self.used) & (1 << n) == 0 {
                *local |= 1 << n;
                return Some(n);
            }
        }
        None
    }

    fn free(&mut self, counter: Counter) {
        match counter {
            Counter::Cycle => self.cycle_used = false,
            Counter::Programmable(n) => {
                if n < 32 {
                    self.used &= !(1 << n);
                }
            }
        }
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

/// Prefers the architectural cycle counter and falls back to a programmable
/// counter carrying the same ARM event, matching `armv8pmu_get_event_idx()`.
pub(super) fn alloc_preferred_cycle(
    event: u16,
    num_counters: usize,
) -> crate::StarryResult<Counter> {
    if let Some(counter) = alloc_cycle_counter() {
        return Ok(counter);
    }
    alloc_programmable(event, num_counters)
}

pub(super) fn free_counter(counter: Counter) {
    ALLOC.lock().free(counter);
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

/// Reserves a fixed programmable slot across every possible task CPU.
pub(super) fn alloc_programmable(event: u16, num_counters: usize) -> crate::StarryResult<Counter> {
    if !crate::perf::event_map::event_supported(event) {
        warn!(
            "perf_event_open: ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(crate::StarryError::Unsupported);
    }
    let Some(Counter::Programmable(n)) = ALLOC.lock().alloc_counter(num_counters) else {
        return Err(crate::StarryError::ResourceBusy);
    };
    Ok(Counter::Programmable(n))
}

/// Atomically reserves a CPU-local slot against all fixed reservations.
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
    fn fixed_reservation_cannot_alias_a_live_flexible_slot() {
        let _guard = crate::sync::NoPreemptIrqSave::new();
        let info = super::super::percpu::ensure_current_cpu_initialized().unwrap();
        let flexible = super::super::percpu::alloc_current_programmable().unwrap();
        let fixed = super::alloc_programmable(0x11, info.num_counters).unwrap();
        let distinct = fixed.programmable_index() != Some(flexible);
        super::free_counter(fixed);
        super::super::percpu::free_current_programmable(flexible);
        assert!(
            distinct,
            "fixed allocation must see the live flexible reservation"
        );
    }
}

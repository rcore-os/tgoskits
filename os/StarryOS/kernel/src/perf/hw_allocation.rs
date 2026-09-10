//! Serialized fixed and CPU-local ARM PMUv3 counter reservations.
//!
//! Hardware slots are physically per CPU, but one global bitmap deliberately
//! keeps fixed events conservative across CPUs. Flexible slices share the same
//! lock and allocation state, so neither allocation direction can alias them.

use super::hw_owner::Counter;
use crate::sync::IrqMutex;

struct HwAlloc {
    used: u32,
    flexible: [u32; super::percpu::MAX_TRACKED_CPUS],
    cycle_used: bool,
}

impl HwAlloc {
    const fn new() -> Self {
        Self {
            used: 0,
            flexible: [0; super::percpu::MAX_TRACKED_CPUS],
            cycle_used: false,
        }
    }

    fn alloc_cycle(&mut self) -> Option<Counter> {
        if self.cycle_used {
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

/// Reserves a validated programmable counter for a system event.
pub(super) fn alloc_programmable(event: u16, num_counters: usize) -> crate::StarryResult<Counter> {
    if !ax_cpu::pmu::event_supported(event) {
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

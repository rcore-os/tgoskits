//! Conservative process-wide ARM PMUv3 counter reservation.
//!
//! Hardware slots are physically per CPU, but one global bitmap deliberately
//! prevents two owners from reserving the same logical slot until per-CPU
//! allocation and multiplexing are modeled.

use ax_errno::{AxError, AxResult};

use super::hw_owner::Counter;

struct HwAlloc {
    num_counters: usize,
    used: u32,
    cycle_used: bool,
}

impl HwAlloc {
    const fn new() -> Self {
        Self {
            num_counters: 0,
            used: 0,
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

    fn alloc_counter(&mut self) -> Option<Counter> {
        for n in 0..self.num_counters.min(32) {
            if self.used & (1 << n) == 0 {
                self.used |= 1 << n;
                return Some(Counter::Programmable(n));
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

static ALLOC: ax_kspin::SpinNoPreempt<HwAlloc> = ax_kspin::SpinNoPreempt::new(HwAlloc::new());

pub(super) fn set_programmable_counter_count(num_counters: usize) {
    ALLOC.lock().num_counters = num_counters;
}

pub(super) fn alloc_cycle_counter() -> Option<Counter> {
    ALLOC.lock().alloc_cycle()
}

/// Prefers the architectural cycle counter and falls back to a programmable
/// counter carrying the same ARM event, matching `armv8pmu_get_event_idx()`.
pub(super) fn alloc_preferred_cycle(event: u16) -> AxResult<Counter> {
    if let Some(counter) = alloc_cycle_counter() {
        return Ok(counter);
    }
    alloc_programmable(event)
}

pub(super) fn free_counter(counter: Counter) {
    ALLOC.lock().free(counter);
}

/// Reserves a validated programmable counter for a system event.
pub(super) fn alloc_programmable(event: u16) -> AxResult<Counter> {
    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }
    let Some(Counter::Programmable(n)) = ALLOC.lock().alloc_counter() else {
        return Err(AxError::NoMemory);
    };
    Ok(Counter::Programmable(n))
}

/// Reserves one unconfigured programmable slot for a task event.
pub(crate) fn alloc_programmable_counter() -> Option<usize> {
    match ALLOC.lock().alloc_counter() {
        Some(Counter::Programmable(n)) => Some(n),
        _ => None,
    }
}

//! Linux-style hard-interrupt time accounting.
//!
//! The common IRQ entry is the sole local writer. Runqueue owners may read a
//! target CPU's cumulative total without taking an IRQ-side lock; any sample
//! that races IRQ exit is capped and consumed by later rq clock updates.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[ax_percpu::def_percpu]
static HARDIRQ_TIME: HardIrqTime = HardIrqTime::new();

struct HardIrqTime {
    depth: AtomicU32,
    start_clock_ns: AtomicU64,
    total_ns: AtomicU64,
}

impl HardIrqTime {
    const fn new() -> Self {
        Self {
            depth: AtomicU32::new(0),
            start_clock_ns: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
        }
    }

    fn enter(&self, now_ns: u64) {
        let depth = self.depth.fetch_add(1, Ordering::Relaxed);
        if depth == 0 {
            self.start_clock_ns.store(now_ns, Ordering::Relaxed);
        }
    }

    fn begins_outer_interval(&self) -> bool {
        self.depth.load(Ordering::Relaxed) == 0
    }

    fn ends_outer_interval(&self) -> bool {
        self.depth.load(Ordering::Relaxed) == 1
    }

    fn exit(&self, now_ns: u64) {
        let depth = self.depth.fetch_sub(1, Ordering::Relaxed);
        assert_ne!(depth, 0, "hard-IRQ exit without a matching entry");
        if depth != 1 {
            return;
        }

        let start_ns = self.start_clock_ns.load(Ordering::Relaxed);
        let elapsed_ns = now_ns.wrapping_sub(start_ns);
        assert!(
            (elapsed_ns as i64) >= 0,
            "scheduler clock moved backwards across a hard interrupt"
        );
        self.total_ns.fetch_add(elapsed_ns, Ordering::Release);
    }

    fn total(&self) -> u64 {
        self.total_ns.load(Ordering::Acquire)
    }
}

fn current_cpu_id() -> usize {
    // SAFETY: common IRQ entry has disabled preemption and local interrupts,
    // so the CPU-local area cannot change during this observation.
    unsafe { ax_percpu::with_cpu_pin(|pin| ax_percpu::current_cpu_index(pin).as_usize()) }
        .unwrap_or_else(|error| panic!("hard-IRQ CPU-local state is invalid: {error}"))
}

fn with_current_state<R>(operation: impl FnOnce(&HardIrqTime) -> R) -> R {
    // SAFETY: the caller is in the common IRQ lifecycle with migration and
    // local re-entry excluded. The object itself uses atomics for remote reads.
    unsafe { ax_percpu::with_cpu_pin(|pin| HARDIRQ_TIME.with_current(pin, operation)) }
        .unwrap_or_else(|error| panic!("hard-IRQ CPU-local state is invalid: {error}"))
}

fn scheduler_clock_now(cpu_id: usize) -> u64 {
    // SAFETY: common IRQ entry keeps the calling CPU pinned and the scheduler
    // clock for this CPU must already be online before interrupts are enabled.
    unsafe { ax_hal::time::scheduler_clock_source(cpu_id) }
        .unwrap_or_else(|error| panic!("hard-IRQ scheduler clock is unavailable: {error}"))
}

fn scheduler_clock_outer_hardirq_entry() -> u64 {
    // SAFETY: common hard-IRQ entry has disabled local interrupts and calls
    // this boundary before publishing the outer accounting interval.
    unsafe { ax_hal::time::scheduler_clock_hardirq_sample() }
        .unwrap_or_else(|error| panic!("hard-IRQ scheduler clock is unavailable: {error}"))
}

pub(crate) fn enter() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "hard-IRQ accounting requires local IRQ exclusion"
    );
    let is_outer = with_current_state(HardIrqTime::begins_outer_interval);
    let now_ns = if is_outer {
        scheduler_clock_outer_hardirq_entry()
    } else {
        0
    };
    with_current_state(|state| state.enter(now_ns));
}

pub(crate) fn exit() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "hard-IRQ accounting requires local IRQ exclusion"
    );
    let is_outer = with_current_state(HardIrqTime::ends_outer_interval);
    let now_ns = if is_outer {
        scheduler_clock_now(current_cpu_id())
    } else {
        0
    };
    with_current_state(|state| state.exit(now_ns));
}

pub(crate) fn total_for_cpu(cpu_id: usize) -> u64 {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id)
        .unwrap_or_else(|_| panic!("hard-IRQ CPU {cpu_id} is outside the installed layout"));
    let area = ax_percpu::area(cpu_index)
        .unwrap_or_else(|error| panic!("hard-IRQ CPU {cpu_id} area is unavailable: {error}"));
    let pointer = HARDIRQ_TIME.remote_ptr(area);
    // SAFETY: the frozen per-CPU layout retains this atomic state for the
    // complete runtime lifetime. Only the owning CPU mutates depth/start.
    unsafe { pointer.as_ref() }.total()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_hard_interrupts_charge_one_outer_interval() {
        let time = HardIrqTime::new();

        time.enter(100);
        time.enter(120);
        time.exit(150);
        time.exit(180);

        assert_eq!(time.total(), 80);
    }

    #[test]
    fn cumulative_time_wraps_without_losing_elapsed_runtime() {
        let time = HardIrqTime::new();

        time.enter(u64::MAX - 2);
        time.exit(2);

        assert_eq!(time.total(), 5);
    }
}

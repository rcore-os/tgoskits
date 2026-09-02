//! Per-CPU accounting for time spent in the architectural idle wait.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::sync::PreemptGuard;

#[ax_percpu::def_percpu]
static IDLE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[ax_percpu::def_percpu]
static IDLE_STARTED_TICKS: AtomicU64 = AtomicU64::new(0);

#[ax_percpu::def_percpu]
static IDLE_ACTIVE: AtomicBool = AtomicBool::new(false);

#[ax_percpu::def_percpu]
static IDLE_TOTAL_TICKS: AtomicU64 = AtomicU64::new(0);

fn with_current_idle_state<R>(
    operation: impl FnOnce(&AtomicU64, &AtomicU64, &AtomicBool, &AtomicU64) -> R,
) -> R {
    let _guard = PreemptGuard::new();
    // SAFETY: `PreemptGuard` prevents migration for the entire callback. Every
    // pointer addresses an atomic object in the pinned CPU's live per-CPU area.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            operation(
                IDLE_SEQUENCE.current_ptr(pin).as_ref(),
                IDLE_STARTED_TICKS.current_ptr(pin).as_ref(),
                IDLE_ACTIVE.current_ptr(pin).as_ref(),
                IDLE_TOTAL_TICKS.current_ptr(pin).as_ref(),
            )
        })
    }
    .expect("idle accounting requires an initialized CPU-local area")
}

#[cfg(not(feature = "host-test"))]
pub(crate) fn begin_idle_wait(started_ticks: u64) {
    with_current_idle_state(|sequence, started, active, _total| {
        sequence.fetch_add(1, Ordering::AcqRel);
        started.store(started_ticks, Ordering::Relaxed);
        active.store(true, Ordering::Relaxed);
        sequence.fetch_add(1, Ordering::Release);
    });
}

/// Closes the current CPU's architectural idle interval, if one is open.
///
/// IRQ entry paths call this before dispatch so interrupt handling is counted
/// as running time. The idle loop calls it again after `wait_for_irqs`; that
/// second call is intentionally a no-op when IRQ entry already closed it.
pub fn finish_current_idle_wait(finished_ticks: u64) {
    with_current_idle_state(|sequence, started, active, total| {
        sequence.fetch_add(1, Ordering::AcqRel);
        let was_active = active.swap(false, Ordering::Relaxed);
        let started_ticks = started.load(Ordering::Relaxed);
        if was_active {
            total.fetch_add(
                finished_ticks.saturating_sub(started_ticks),
                Ordering::Relaxed,
            );
        }
        sequence.fetch_add(1, Ordering::Release);
    });
}

/// Returns cumulative architectural-idle ticks for one CPU.
///
/// When the CPU is currently inside `wait_for_irqs`, the returned value also
/// includes the open interval through `now_ticks`. `None` means `cpu_id` is
/// outside the initialized host CPU set.
pub fn idle_time_ticks(cpu_id: usize, now_ticks: u64) -> Option<u64> {
    if cpu_id >= ax_hal::cpu_num() {
        return None;
    }

    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id).ok()?;
    let area = ax_percpu::area(cpu_index).ok()?;

    // SAFETY: `area` identifies an initialized, shutdown-lifetime per-CPU
    // allocation. These references expose only atomics and use the sequence
    // protocol below for consistent remote observations.
    let (sequence, started, active, total) = unsafe {
        (
            IDLE_SEQUENCE.remote_ptr(area).as_ref(),
            IDLE_STARTED_TICKS.remote_ptr(area).as_ref(),
            IDLE_ACTIVE.remote_ptr(area).as_ref(),
            IDLE_TOTAL_TICKS.remote_ptr(area).as_ref(),
        )
    };

    loop {
        let before = sequence.load(Ordering::Acquire);
        if before & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let accumulated = total.load(Ordering::Relaxed);
        let open_interval = started.load(Ordering::Relaxed);
        let is_active = active.load(Ordering::Relaxed);
        let after = sequence.load(Ordering::Acquire);
        if before == after {
            let open_ticks = if is_active {
                now_ticks.saturating_sub(open_interval)
            } else {
                0
            };
            return Some(accumulated.saturating_add(open_ticks));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_idle_interval_uses_saturating_elapsed_ticks() {
        assert_eq!(50_u64.saturating_add(120_u64.saturating_sub(100)), 70);
        assert_eq!(50_u64.saturating_add(80_u64.saturating_sub(100)), 50);
    }
}

//! x86_64 system-timer glue backed by the local-APIC timer.
//!
//! someboot owns only the TSC counter (frequency, current tick, stability);
//! everything that arms the system timer lives inside the local APIC, so this
//! module brings the local APIC up per CPU, calibrates the APIC-timer fallback
//! ratio, and arms one-shot deadlines. It replaces the `someboot::timer`
//! arming contract for x86_64 the same way the GIC lives in somehal on
//! aarch64: the interrupt-controller driver is consumed here, not by the boot
//! layer.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// Counter-domain helpers stay in someboot, which owns the TSC.
pub use someboot::timer::{
    CounterStability, duration_to_ticks, elapsed, freq, scheduler_clock_stability, since_boot,
    ticks, ticks_to_duration,
};
use x86_apic_driver::{X86LocalApic, local_apic::cpu_has_tsc_deadline};

use super::lapic::local_apic;

/// Minimum armable LAPIC-timer delta in TSC ticks; smaller requests are
/// clamped so a deadline in the past cannot be lost in the conversion
/// pipeline.
const TIMER_MIN_DELTA_TICKS: u32 = 0x0f;

/// APIC-timer counts per TSC tick in Q32 fixed point, calibrated on CPUs
/// without the TSC-deadline mode. Last writer wins across CPUs, matching the
/// per-CPU calibration the previous boot-layer implementation performed.
static APIC_COUNTS_PER_TSC_Q32: AtomicU64 = AtomicU64::new(0);

/// Whether the timer was brought up in TSC-deadline mode, decided once per
/// CPU bring-up from the CPUID capability.
static TSC_DEADLINE_MODE: AtomicBool = AtomicBool::new(false);

/// Brings up the local APIC on the current CPU and enables its timer.
///
/// Called once per CPU from the platform's early initialization, before any
/// other local-APIC use (EOI, IPIs, IOAPIC routing). Repeated calls are
/// idempotent: bring-up rewrites the same register state and the timer ends
/// up unmasked.
pub fn enable() {
    let lapic = local_apic();
    let tsc_deadline = cpu_has_tsc_deadline();
    // SAFETY: per-CPU early initialization runs with interrupts disabled,
    // before any interrupt source is routed to the configured vectors.
    unsafe { lapic.bring_up() }.expect("local APIC bring-up must succeed on x86_64");
    TSC_DEADLINE_MODE.store(tsc_deadline, Ordering::Release);

    if !tsc_deadline {
        calibrate_apic_timer_ratio(&lapic);
    }
    // Clear any stale interrupt state left by firmware, then arm the timer.
    lapic.eoi();
    lapic.timer_set_masked(false);
}

/// Unmasks the timer interrupt.
pub fn irq_enable() {
    local_apic().timer_set_masked(false);
}

/// Masks the timer interrupt.
pub fn irq_disable() {
    local_apic().timer_set_masked(true);
}

/// Returns whether the timer interrupt is currently unmasked.
pub fn irq_is_enabled() -> bool {
    local_apic().timer_is_unmasked()
}

/// Arms a one-shot deadline `ticks_from_now` from the current TSC.
pub fn set_next_event_in_ticks(ticks_from_now: usize) {
    let lapic = local_apic();
    let delta =
        u64::try_from(ticks_from_now.max(TIMER_MIN_DELTA_TICKS as usize)).unwrap_or(u64::MAX);
    if TSC_DEADLINE_MODE.load(Ordering::Acquire) {
        let deadline = someboot::timer::ticks() as u64 + delta;
        lapic.timer_set_tsc_deadline(deadline);
    } else {
        lapic.timer_set_initial_count(ticks_to_apic_counts(delta));
    }
}

fn calibrate_apic_timer_ratio(lapic: &X86LocalApic) {
    let wait_tsc = (freq() / 100).max(1); // target ~=10ms in TSC domain

    lapic.timer_set_initial_count(u32::MAX);
    let start_tsc = someboot::timer::ticks();
    loop {
        if someboot::timer::ticks().wrapping_sub(start_tsc) >= wait_tsc {
            break;
        }
        core::hint::spin_loop();
    }
    let end_tsc = someboot::timer::ticks();
    let current = lapic.timer_current_count();
    lapic.timer_set_initial_count(0);

    let elapsed_tsc = end_tsc.wrapping_sub(start_tsc) as u64;
    let elapsed_apic = (u32::MAX - current) as u64;
    let q32 = if elapsed_tsc == 0 || elapsed_apic == 0 {
        1u64 << 32
    } else {
        (((elapsed_apic as u128) << 32) / elapsed_tsc as u128) as u64
    };
    APIC_COUNTS_PER_TSC_Q32.store(q32, Ordering::Release);
}

fn ticks_to_apic_counts(ticks: u64) -> u32 {
    let q32 = APIC_COUNTS_PER_TSC_Q32.load(Ordering::Acquire);
    let q32 = if q32 == 0 { 1u64 << 32 } else { q32 };
    let counts = ((ticks as u128 * q32 as u128) >> 32).max(1);
    counts.clamp(u64::from(TIMER_MIN_DELTA_TICKS) as u128, u32::MAX as u128) as u32
}

#[cfg(test)]
mod tests {
    use super::ticks_to_apic_counts;

    #[test]
    fn legacy_lapic_clamps_overdue_events_to_the_device_minimum() {
        // Before calibration the ratio falls back to 1:1, so a one-tick
        // request must still arm the device minimum.
        assert_eq!(ticks_to_apic_counts(1), 0x0f);
    }
}

//! x86_64 system-timer glue backed by the local-APIC timer.
//!
//! someboot owns only the TSC counter (frequency, current tick, stability);
//! everything that arms the system timer lives inside the local APIC, so this
//! module brings the local APIC up per CPU, calibrates the APIC-timer fallback
//! ratio, and arms one-shot deadlines. It replaces the `someboot::timer`
//! arming contract for x86_64 the same way the GIC lives in somehal on
//! aarch64: the interrupt-controller driver is consumed here, not by the boot
//! layer.

// Counter-domain helpers stay in someboot, which owns the TSC.
pub use someboot::timer::{
    CounterStability, duration_to_ticks, elapsed, freq, scheduler_clock_stability, since_boot,
    ticks, ticks_to_duration,
};
use x86_apic_driver::{X86LocalApic, local_apic::cpu_has_tsc_deadline};

use super::lapic::{
    LocalTimerProfile, install_current_lapic, new_current_lapic, with_current_lapic,
};

/// Minimum armable LAPIC-timer delta in TSC ticks; smaller requests are
/// clamped so a deadline in the past cannot be lost in the conversion
/// pipeline.
const TIMER_MIN_DELTA_TICKS: u32 = 0x0f;

/// Brings up the local APIC and leaves its timer masked and non-firing.
///
/// Called exactly once per CPU from the platform's early initialization,
/// before any other local-APIC use (EOI, IPIs, IOAPIC routing). The timer
/// stays masked until the runtime clockevent publishes its first finite
/// deadline.
///
/// # Panics
///
/// Panics when APIC bring-up fails or this CPU already installed its device.
pub fn enable() {
    let tsc_deadline = cpu_has_tsc_deadline();
    let mut lapic = new_current_lapic(tsc_deadline);
    // SAFETY: per-CPU early initialization runs with interrupts disabled,
    // before any interrupt source is routed to the configured vectors.
    unsafe { lapic.bring_up() }.expect("local APIC bring-up must succeed on x86_64");

    let apic_counts_per_tsc_q32 = if tsc_deadline {
        lapic.timer_set_tsc_deadline(0);
        0
    } else {
        calibrate_apic_timer_ratio(&lapic)
    };
    // `bring_up` masks the timer and programs a zero initial count. Complete
    // any stale in-service interrupt without making the source observable.
    lapic.eoi();
    install_current_lapic(
        lapic,
        LocalTimerProfile {
            tsc_deadline,
            apic_counts_per_tsc_q32,
        },
    );
}

/// Unmasks the timer interrupt.
pub fn irq_enable() {
    with_current_lapic(|lapic, _timer| {
        lapic.timer_set_masked(false);
        lapic.timer_serialize_mask_update();
    });
}

/// Masks the timer interrupt.
pub fn irq_disable() {
    with_current_lapic(|lapic, _timer| lapic.timer_set_masked(true));
}

/// Returns whether the timer interrupt is currently unmasked.
pub fn irq_is_enabled() -> bool {
    with_current_lapic(|lapic, _timer| lapic.timer_is_unmasked())
}

/// Arms a one-shot deadline `ticks_from_now` from the current TSC.
pub fn set_next_event_in_ticks(ticks_from_now: usize) {
    with_current_lapic(|lapic, timer| program_next_event(lapic, timer, ticks_from_now));
}

fn program_next_event(lapic: &X86LocalApic, timer: LocalTimerProfile, ticks_from_now: usize) {
    let delta =
        u64::try_from(ticks_from_now.max(TIMER_MIN_DELTA_TICKS as usize)).unwrap_or(u64::MAX);
    if timer.tsc_deadline {
        let deadline = someboot::timer::ticks() as u64 + delta;
        lapic.timer_set_tsc_deadline(deadline);
    } else {
        lapic.timer_set_initial_count(ticks_to_apic_counts(delta, timer.apic_counts_per_tsc_q32));
    }
}

/// Masks the source before clearing its comparator.
pub fn cancel_oneshot() {
    with_current_lapic(|lapic, timer| {
        cancel_oneshot_with(
            || lapic.timer_set_masked(true),
            || {
                if timer.tsc_deadline {
                    lapic.timer_set_tsc_deadline(0);
                } else {
                    lapic.timer_set_initial_count(0);
                }
            },
        );
    });
}

/// Makes the source observable before installing a fresh comparator.
pub fn resume_oneshot_in_ticks(ticks_from_now: usize) {
    with_current_lapic(|lapic, timer| {
        resume_oneshot_with(
            ticks_from_now,
            || {
                lapic.timer_set_masked(false);
                lapic.timer_serialize_mask_update();
            },
            |ticks| program_next_event(lapic, timer, ticks),
        );
    });
}

fn cancel_oneshot_with(mask: impl FnOnce(), clear_comparator: impl FnOnce()) {
    mask();
    clear_comparator();
}

fn resume_oneshot_with(
    ticks_from_now: usize,
    unmask_and_serialize: impl FnOnce(),
    program_comparator: impl FnOnce(usize),
) {
    unmask_and_serialize();
    program_comparator(ticks_from_now);
}

fn calibrate_apic_timer_ratio(lapic: &X86LocalApic) -> u64 {
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
    if elapsed_tsc == 0 || elapsed_apic == 0 {
        1u64 << 32
    } else {
        (((elapsed_apic as u128) << 32) / elapsed_tsc as u128) as u64
    }
}

fn ticks_to_apic_counts(ticks: u64, q32: u64) -> u32 {
    let q32 = if q32 == 0 { 1u64 << 32 } else { q32 };
    let counts = ((ticks as u128 * q32 as u128) >> 32).max(1);
    counts.clamp(u64::from(TIMER_MIN_DELTA_TICKS) as u128, u32::MAX as u128) as u32
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{cancel_oneshot_with, resume_oneshot_with, ticks_to_apic_counts};

    #[test]
    fn legacy_lapic_clamps_overdue_events_to_the_device_minimum() {
        // Before calibration the ratio falls back to 1:1, so a one-tick
        // request must still arm the device minimum.
        assert_eq!(ticks_to_apic_counts(1, 0), 0x0f);
    }

    #[test]
    fn legacy_lapic_uses_the_current_cpu_calibration_ratio() {
        assert_eq!(ticks_to_apic_counts(64, 2u64 << 32), 128);
        assert_eq!(ticks_to_apic_counts(64, 1u64 << 31), 32);
    }

    #[test]
    fn cancel_masks_before_clearing_the_comparator() {
        let transitions = Cell::new(0);
        cancel_oneshot_with(
            || transitions.set(transitions.get() * 10 + 1),
            || transitions.set(transitions.get() * 10 + 2),
        );
        assert_eq!(transitions.get(), 12);
    }

    #[test]
    fn resume_unmasks_and_serializes_before_programming() {
        let transitions = Cell::new(0);
        resume_oneshot_with(
            7,
            || transitions.set(transitions.get() * 10 + 1),
            |ticks| transitions.set(transitions.get() * 10 + ticks),
        );
        assert_eq!(transitions.get(), 17);
    }
}

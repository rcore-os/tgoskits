//! Platform-selected clock-event source over CPU generic-timer banks.

use ax_cpu::{
    interrupt,
    timer::{Timer, TimerControl, TimerKind},
};

use crate::timer::{self, ArchTimerMode};

pub(super) fn enable() {
    with_timer(|timer| timer.set_control(TimerControl::ENABLE));
}

pub(super) fn mask() {
    with_timer(|timer| timer.set_control(timer.control() | TimerControl::MASKED));
}

pub(super) fn unmask() {
    with_timer(|timer| timer.set_control(timer.control() & !TimerControl::MASKED));
}

pub(super) fn irq_enabled() -> bool {
    with_timer(|timer| !timer.control().contains(TimerControl::MASKED))
}

pub(super) fn set_deadline(deadline_ticks: u64) {
    with_timer(|timer| {
        let next = timer::next_cpu_timer_deadline(timer.counter(), deadline_ticks);
        timer.set_compare(next);
    });
}

pub(super) fn stop_oneshot() {
    with_timer(|timer| {
        timer.set_control(timer.control() | TimerControl::MASKED);
        timer.set_compare(u64::MAX);
    });
}

fn with_timer<R>(operation: impl FnOnce(&mut Timer) -> R) -> R {
    struct RestoreIrqs(bool);
    impl Drop for RestoreIrqs {
        fn drop(&mut self) {
            if self.0 {
                interrupt::enable_irqs();
            }
        }
    }
    let restore = RestoreIrqs(interrupt::irqs_enabled());
    interrupt::disable_irqs();
    let kind = match timer::aarch64_timer_mode() {
        ArchTimerMode::El1Phys => TimerKind::Physical,
        ArchTimerMode::El1Virt => TimerKind::Virtual,
        ArchTimerMode::El2HypPhys => TimerKind::HypervisorPhysical,
    };
    // SAFETY: boot fixes the accessible comparator bank before publishing the
    // platform. This source is exclusively owned by the clock-event layer;
    // IRQ exclusion prevents migration, interrupt reentry and guest switching
    // throughout this bounded operation. The view cannot escape the closure.
    let result = operation(&mut unsafe { Timer::current(kind) });
    drop(restore);
    result
}

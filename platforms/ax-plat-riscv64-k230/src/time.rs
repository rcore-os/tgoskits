use ax_plat::time::{NANOS_PER_SEC, TimeIf};
use riscv::register::time;

const NANOS_PER_TICK: u64 = NANOS_PER_SEC / crate::config::devices::TIMER_FREQUENCY as u64;
/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub(super) fn init_early() {}

pub(super) fn init_percpu() {
    #[cfg(feature = "irq")]
    sbi_rt::set_timer(0);
}

struct TimeIfImpl;

#[impl_plat_interface]
impl TimeIf for TimeIfImpl {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        time::read() as u64
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks * NANOS_PER_TICK
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos / NANOS_PER_TICK
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic clock start).
    fn epochoffset_nanos() -> u64 {
        unsafe { RTC_EPOCHOFFSET_NANOS }
    }

    /// Returns the IRQ number for the timer interrupt.
    #[cfg(feature = "irq")]
    fn irq_num() -> usize {
        crate::config::devices::TIMER_IRQ
    }

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64) {
        sbi_rt::set_timer(Self::nanos_to_ticks(deadline_ns));
    }
}

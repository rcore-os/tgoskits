use riscv::register::time;

use axplat::time::TimeIf;

const NANOS_PER_SEC: u64 = 1_000_000_000;

const NANOS_PER_TICK: u64 = NANOS_PER_SEC / crate::config::devices::TIMER_FREQUENCY as u64;
/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub(super) fn init_early() {
    #[cfg(feature = "rtc")]
    use crate::config::devices::RTC_PADDR;

    #[cfg(feature = "rtc")]
    if RTC_PADDR != 0 {
        use axplat::mem::{PhysAddr, pa, phys_to_virt};

        use riscv_goldfish::Rtc;

        const GOLDFISH_BASE: PhysAddr = pa!(RTC_PADDR);
        // Get the current time in microseconds since the epoch (1970-01-01) from the riscv RTC.
        // Subtract the timer ticks to get the actual time when ArceOS was booted.
        let epoch_time_nanos =
            Rtc::new(phys_to_virt(GOLDFISH_BASE).as_usize()).get_unix_timestamp() * 1_000_000_000;

        unsafe {
            RTC_EPOCHOFFSET_NANOS =
                epoch_time_nanos - TimeIfImpl::ticks_to_nanos(TimeIfImpl::current_ticks());
        }
    }
}

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

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64) {
        sbi_rt::set_timer(Self::nanos_to_ticks(deadline_ns));
    }
}

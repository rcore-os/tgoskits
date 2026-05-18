use ax_plat::time::{NANOS_PER_SEC, TimeIf};
use riscv::register::time;

const NANOS_PER_TICK: u64 = NANOS_PER_SEC / crate::config::devices::TIMER_FREQUENCY as u64;
/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub(super) fn init_early() {
    #[cfg(feature = "rtc")]
    use crate::config::devices::RTC_PADDR;

    #[cfg(feature = "rtc")]
    if RTC_PADDR != 0 {
        use ax_plat::mem::{pa, phys_to_virt};

        // Get the current time in seconds since the epoch (1970-01-01) from the SG2002 RTC.
        // Subtract the timer ticks to get the actual time when ArceOS was booted.
        let epoch_time_nanos =
            read_sg2002_rtc_seconds(phys_to_virt(pa!(RTC_PADDR)).as_usize()) * 1_000_000_000;

        unsafe {
            RTC_EPOCHOFFSET_NANOS =
                epoch_time_nanos - TimeIfImpl::ticks_to_nanos(TimeIfImpl::current_ticks());
        }
    }
}

#[cfg(feature = "rtc")]
fn read_sg2002_rtc_seconds(base_vaddr: usize) -> u64 {
    const CVI_RTC_SEC_CNTR_VALUE: usize = 0x18;
    const RTC_MACRO_RO_T: usize = 0x4A8;
    const VALID_TIME_THRESHOLD: u32 = 0x3000_0000;

    let rtc_base = base_vaddr as *const u8;
    let sec =
        unsafe { core::ptr::read_volatile(rtc_base.add(CVI_RTC_SEC_CNTR_VALUE) as *const u32) };
    let sec_ro_t = unsafe { core::ptr::read_volatile(rtc_base.add(RTC_MACRO_RO_T) as *const u32) };

    let sec = if sec_ro_t > VALID_TIME_THRESHOLD {
        sec_ro_t
    } else {
        sec
    };

    sec as u64
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

//! PL031 Real Time Clock (RTC) driver.

use arm_pl031::Rtc;
use axplat::mem::VirtAddr;

use crate::generic_timer::{current_ticks, ticks_to_nanos};

/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

/// Return epoch offset in nanoseconds (wall time offset to monotonic clock start).
#[inline]
pub fn epochoffset_nanos() -> u64 {
    unsafe { RTC_EPOCHOFFSET_NANOS }
}

/// Early stage initialization of the RTC driver.
///
/// It reads the current real time and calculates the epoch offset.
pub fn init_early(rtc_base: VirtAddr) {
    // Make sure `RTC_PADDR` is valid in platform config file.
    if rtc_base.as_usize() == 0 {
        return;
    }

    let rtc = unsafe { Rtc::new(rtc_base.as_mut_ptr() as _) };

    // Get the current time in microseconds since the epoch (1970-01-01) from the aarch64 pl031 RTC.
    // Subtract the timer ticks to get the actual time when ArceOS was booted.
    let epoch_time_nanos = rtc.get_unix_timestamp() as u64 * 1_000_000_000;

    unsafe {
        RTC_EPOCHOFFSET_NANOS = epoch_time_nanos - ticks_to_nanos(current_ticks());
    }
}

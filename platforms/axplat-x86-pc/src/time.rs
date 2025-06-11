//! Time management.
//!
//! Currently, the TSC is used as the clock source.

use axplat::time::TimeIf;
use raw_cpuid::CpuId;

#[cfg(feature = "irq")]
use int_ratio::Ratio;

#[cfg(feature = "irq")]
const LAPIC_TICKS_PER_SEC: u64 = 1_000_000_000; // TODO: need to calibrate

#[cfg(feature = "irq")]
static mut NANOS_TO_LAPIC_TICKS_RATIO: Ratio = Ratio::zero();

static mut INIT_TICK: u64 = 0;
static mut CPU_FREQ_MHZ: u64 = crate::config::devices::TIMER_FREQUENCY as u64 / 1_000_000;

/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub fn init_early() {
    if let Some(freq) = CpuId::new()
        .get_processor_frequency_info()
        .map(|info| info.processor_base_frequency())
        && freq > 0
    {
        unsafe { CPU_FREQ_MHZ = freq as u64 }
    }

    axplat::console_println!("TSC frequency: {} MHz", unsafe { CPU_FREQ_MHZ });

    unsafe {
        INIT_TICK = core::arch::x86_64::_rdtsc();
    }

    #[cfg(feature = "rtc")]
    {
        use x86_rtc::Rtc;

        // Get the current time in microseconds since the epoch (1970-01-01) from the x86 RTC.
        // Subtract the timer ticks to get the actual time when ArceOS was booted.
        let eopch_time_nanos = Rtc::new().get_unix_timestamp() * 1_000_000_000;
        unsafe {
            RTC_EPOCHOFFSET_NANOS = eopch_time_nanos - axplat::time::ticks_to_nanos(INIT_TICK);
        }
    }
}

pub fn init_primary() {
    #[cfg(feature = "irq")]
    unsafe {
        use x2apic::lapic::{TimerDivide, TimerMode};
        let lapic = super::apic::local_apic();
        lapic.set_timer_mode(TimerMode::OneShot);
        lapic.set_timer_divide(TimerDivide::Div256); // indeed it is Div1, the name is confusing.
        lapic.enable_timer();

        // TODO: calibrate with HPET
        NANOS_TO_LAPIC_TICKS_RATIO = Ratio::new(
            LAPIC_TICKS_PER_SEC as u32,
            axplat::time::NANOS_PER_SEC as u32,
        );
    }
}

#[cfg(feature = "smp")]
pub fn init_secondary() {
    #[cfg(feature = "irq")]
    unsafe {
        crate::apic::local_apic().enable_timer();
    }
}

struct TimeIfImpl;

#[impl_plat_interface]
impl TimeIf for TimeIfImpl {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() - INIT_TICK }
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks * 1_000 / unsafe { CPU_FREQ_MHZ }
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos * unsafe { CPU_FREQ_MHZ } / 1_000
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64 {
        unsafe { RTC_EPOCHOFFSET_NANOS }
    }

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds).
    fn set_oneshot_timer(deadline_ns: u64) {
        #[cfg(feature = "irq")]
        {
            let lapic = super::apic::local_apic();
            let now_ns = Self::ticks_to_nanos(Self::current_ticks());
            unsafe {
                if now_ns < deadline_ns {
                    let apic_ticks = NANOS_TO_LAPIC_TICKS_RATIO.mul_trunc(deadline_ns - now_ns);
                    assert!(apic_ticks <= u32::MAX as u64);
                    lapic.set_timer_initial(apic_ticks.max(1) as u32);
                } else {
                    lapic.set_timer_initial(1);
                }
            }
        }
        #[cfg(not(feature = "irq"))]
        let _ = deadline_ns;
    }
}

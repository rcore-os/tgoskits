use axplat::time::TimeIf;
use lazyinit::LazyInit;
use loongArch64::time::Time;

static NANOS_PER_TICK: LazyInit<u64> = LazyInit::new();

/// RTC wall time offset in nanoseconds at monotonic time base.
static mut RTC_EPOCHOFFSET_NANOS: u64 = 0;

pub(super) fn init_percpu() {
    #[cfg(feature = "irq")]
    {
        use loongArch64::register::tcfg;
        tcfg::set_init_val(0);
        tcfg::set_periodic(false);
        tcfg::set_en(true);
        axplat::irq::set_enable(crate::config::devices::TIMER_IRQ, true);
    }
}

/// Initializes the RTC (Real-Time Clock) device.
///
/// The QEMU-loongson3-virt platform supports loongson7a RTC device, whose documentation can be found at [Loongson7a RTC][1].
///
/// The emulation for RTC in QEMU can be found at [ls7a_rtc.c][2].We will use its TOY counter to provide RTC.
///
/// [1]: https://github.com/loongson/LoongArch-Documentation/releases/latest/download/Loongson-7A1000-usermanual-v2.00-CN.pdf
/// [2]: https://gitlab.com/qemu-project/qemu/-/blob/1cf9bc6eba7506ab6d9de635f224259225f63466/hw/rtc/ls7a_rtc.c
#[cfg(feature = "rtc")]
fn init_rtc() {
    use crate::mem::phys_to_virt;
    use chrono::{TimeZone, Timelike, Utc};
    use memory_addr::PhysAddr;

    const SYS_TOY_READ0: usize = 0x2C;
    const SYS_TOY_READ1: usize = 0x30;
    const SYS_RTCCTRL: usize = 0x40;

    const TOY_ENABLE: u32 = 1 << 11;
    const OSC_ENABLE: u32 = 1 << 8;

    const LS7A_RTC_VADDR: PhysAddr = pa!(crate::config::devices::RTC_PADDR);

    let rtc_base_ptr = phys_to_virt(LS7A_RTC_VADDR).as_mut_ptr();

    fn extract_bits(value: u32, range: core::ops::Range<u32>) -> u32 {
        (value >> range.start) & ((1 << (range.end - range.start)) - 1)
    }

    unsafe {
        // init the TOY counter
        (rtc_base_ptr.add(SYS_RTCCTRL) as *mut u32).write_volatile(TOY_ENABLE | OSC_ENABLE);
    }

    // high-32bit value of the TOY counter, which stores year information
    let toy_high = unsafe { (rtc_base_ptr.add(SYS_TOY_READ1) as *const u32).read_volatile() };

    // low-32bit value of the TOY counter, which stores seconds and other time information
    let toy_low = unsafe { (rtc_base_ptr.add(SYS_TOY_READ0) as *const u32).read_volatile() };

    let date_time = Utc
        .with_ymd_and_hms(
            1900 + toy_high as i32,
            extract_bits(toy_low, 26..32),
            extract_bits(toy_low, 21..26),
            extract_bits(toy_low, 16..21),
            extract_bits(toy_low, 10..16),
            extract_bits(toy_low, 4..10),
        )
        .unwrap()
        .with_nanosecond(extract_bits(toy_low, 0..4) * axplat::time::NANOS_PER_MILLIS as u32)
        .unwrap();

    if let Some(epoch_time_nanos) = date_time.timestamp_nanos_opt() {
        unsafe {
            RTC_EPOCHOFFSET_NANOS =
                epoch_time_nanos as u64 - TimeIfImpl::ticks_to_nanos(TimeIfImpl::current_ticks());
        }
    }
}

pub(super) fn init_early() {
    NANOS_PER_TICK
        .init_once(axplat::time::NANOS_PER_SEC / loongArch64::time::get_timer_freq() as u64);

    #[cfg(feature = "rtc")]
    init_rtc();
}

struct TimeIfImpl;

#[impl_plat_interface]
impl TimeIf for TimeIfImpl {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        Time::read() as _
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic clock start).
    fn epochoffset_nanos() -> u64 {
        unsafe { RTC_EPOCHOFFSET_NANOS }
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks * *NANOS_PER_TICK
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos / *NANOS_PER_TICK
    }

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
    ///
    /// LoongArch64 TCFG CSR: <https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html#timer-configuration>
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64) {
        use loongArch64::register::tcfg;

        let ticks_now = Self::current_ticks();
        let ticks_deadline = Self::nanos_to_ticks(deadline_ns);
        let init_value = ticks_deadline - ticks_now;
        tcfg::set_init_val(init_value as _);
        tcfg::set_en(true);
    }
}

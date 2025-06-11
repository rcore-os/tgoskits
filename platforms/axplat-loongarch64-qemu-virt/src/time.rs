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

pub(super) fn init_early() {
    NANOS_PER_TICK
        .init_once(axplat::time::NANOS_PER_SEC / loongArch64::time::get_timer_freq() as u64);
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
    fn set_oneshot_timer(_deadline_ns: u64) {
        #[cfg(feature = "irq")]
        {
            use loongArch64::register::tcfg;

            let ticks_now = Self::current_ticks();
            let ticks_deadline = Self::nanos_to_ticks(_deadline_ns);
            let init_value = ticks_deadline - ticks_now;
            tcfg::set_init_val(init_value as _);
            tcfg::set_en(true);
        }
    }
}

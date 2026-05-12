//! ARM Generic Timer.

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTV_TVAL_EL0, CNTVCT_EL0, Readable, Writeable};
use ax_int_ratio::Ratio;

static mut CNTVCT_TO_NANOS_RATIO: Ratio = Ratio::zero();
static mut NANOS_TO_CNTVCT_RATIO: Ratio = Ratio::zero();

/// Returns the current clock time in hardware ticks.
#[inline]
pub fn current_ticks() -> u64 {
    CNTVCT_EL0.get()
}

/// Converts hardware ticks to nanoseconds.
#[inline]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    unsafe { CNTVCT_TO_NANOS_RATIO.mul_trunc(ticks) }
}

/// Converts nanoseconds to hardware ticks.
#[inline]
pub fn nanos_to_ticks(nanos: u64) -> u64 {
    unsafe { NANOS_TO_CNTVCT_RATIO.mul_trunc(nanos) }
}

/// Set a one-shot timer.
///
/// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
pub fn set_oneshot_timer(deadline_ns: u64) {
    let cntvct = CNTVCT_EL0.get();
    let cntvct_deadline = nanos_to_ticks(deadline_ns);
    if cntvct < cntvct_deadline {
        let interval = cntvct_deadline - cntvct;
        debug_assert!(interval <= u32::MAX as u64);
        CNTV_TVAL_EL0.set(interval);
    } else {
        CNTV_TVAL_EL0.set(0);
    }
}

/// Early stage initialization: stores the timer frequency.
pub fn init_early() {
    let freq = CNTFRQ_EL0.get();
    unsafe {
        CNTVCT_TO_NANOS_RATIO = Ratio::new(ax_plat::time::NANOS_PER_SEC as u32, freq as u32);
        NANOS_TO_CNTVCT_RATIO = CNTVCT_TO_NANOS_RATIO.inverse();
    }
}

/// Enable timer interrupts.
///
/// It should be called on all CPUs, as the timer interrupt is a PPI (Private
/// Peripheral Interrupt).
#[cfg(feature = "irq")]
pub fn enable_irqs(timer_irq_num: usize) {
    // CNTV (virtual timer) instead of CNTP (physical): in virtualized
    // environments — Apple HVF in particular — EL2 owns CNTP and traps
    // any EL1 access to it with `EC=Unknown`, crashing us. CNTV is the
    // guest-owned timer and is always accessible at EL1. On bare-metal
    // TCG the two tick identically, so this choice is harmless there.
    // IRQ number changes too: CNTV is PPI 11 (IRQ 27), CNTP is PPI 14
    // (IRQ 30); `timer-irq` in axconfig.toml now advertises 27.
    use aarch64_cpu::registers::CNTV_CTL_EL0;
    CNTV_CTL_EL0.write(CNTV_CTL_EL0::ENABLE::SET);
    CNTV_TVAL_EL0.set(0);
    ax_plat::irq::set_enable(timer_irq_num, true);
}

/// Default implementation of [`ax_plat::time::TimeIf`] using the generic
/// timer.
#[macro_export]
#[allow(clippy::crate_in_macro_def)]
macro_rules! time_if_impl {
    ($name:ident) => {
        struct $name;

        #[impl_plat_interface]
        impl ax_plat::time::TimeIf for $name {
            /// Returns the current clock time in hardware ticks.
            fn current_ticks() -> u64 {
                $crate::generic_timer::current_ticks()
            }

            /// Converts hardware ticks to nanoseconds.
            fn ticks_to_nanos(ticks: u64) -> u64 {
                $crate::generic_timer::ticks_to_nanos(ticks)
            }

            /// Converts nanoseconds to hardware ticks.
            fn nanos_to_ticks(nanos: u64) -> u64 {
                $crate::generic_timer::nanos_to_ticks(nanos)
            }

            /// Return epoch offset in nanoseconds (wall time offset to monotonic
            /// clock start).
            fn epochoffset_nanos() -> u64 {
                $crate::pl031::epochoffset_nanos()
            }

            /// Returns the IRQ number for the timer interrupt.
            #[cfg(feature = "irq")]
            fn irq_num() -> usize {
                crate::config::devices::TIMER_IRQ
            }

            /// Set a one-shot timer.
            ///
            /// A timer interrupt will be triggered at the specified monotonic time
            /// deadline (in nanoseconds).
            #[cfg(feature = "irq")]
            fn set_oneshot_timer(deadline_ns: u64) {
                $crate::generic_timer::set_oneshot_timer(deadline_ns)
            }
        }
    };
}

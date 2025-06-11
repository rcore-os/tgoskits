//! ARM Generic Timer.

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTP_CTL_EL0, CNTP_TVAL_EL0, CNTPCT_EL0};
use aarch64_cpu::registers::{Readable, Writeable};
use int_ratio::Ratio;

static mut CNTPCT_TO_NANOS_RATIO: Ratio = Ratio::zero();
static mut NANOS_TO_CNTPCT_RATIO: Ratio = Ratio::zero();

/// Returns the current clock time in hardware ticks.
#[inline]
pub fn current_ticks() -> u64 {
    CNTPCT_EL0.get()
}

/// Converts hardware ticks to nanoseconds.
#[inline]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    unsafe { CNTPCT_TO_NANOS_RATIO.mul_trunc(ticks) }
}

/// Converts nanoseconds to hardware ticks.
#[inline]
pub fn nanos_to_ticks(nanos: u64) -> u64 {
    unsafe { NANOS_TO_CNTPCT_RATIO.mul_trunc(nanos) }
}

/// Set a one-shot timer.
///
/// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
pub fn set_oneshot_timer(deadline_ns: u64) {
    let cnptct = CNTPCT_EL0.get();
    let cnptct_deadline = nanos_to_ticks(deadline_ns);
    if cnptct < cnptct_deadline {
        let interval = cnptct_deadline - cnptct;
        debug_assert!(interval <= u32::MAX as u64);
        CNTP_TVAL_EL0.set(interval);
    } else {
        CNTP_TVAL_EL0.set(0);
    }
}

/// Early stage initialization: stores the timer frequency.
pub fn init_early() {
    let freq = CNTFRQ_EL0.get();
    unsafe {
        CNTPCT_TO_NANOS_RATIO = Ratio::new(axplat::time::NANOS_PER_SEC as u32, freq as u32);
        NANOS_TO_CNTPCT_RATIO = CNTPCT_TO_NANOS_RATIO.inverse();
    }
}

/// Enable timer interrupts.
///
/// It should be called on all CPUs, as the timer interrupt is a PPI (Private
/// Peripheral Interrupt).
pub fn enable_irqs(timer_irq_num: usize) {
    CNTP_CTL_EL0.write(CNTP_CTL_EL0::ENABLE::SET);
    CNTP_TVAL_EL0.set(0);
    crate::gic::set_enable(timer_irq_num, true);
}

/// Default implementation of [`axplat::time::TimeIf`] using the generic
/// timer.
#[macro_export]
macro_rules! time_if_impl {
    ($name:ident) => {
        struct $name;

        #[impl_plat_interface]
        impl axplat::time::TimeIf for $name {
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

            /// Set a one-shot timer.
            ///
            /// A timer interrupt will be triggered at the specified monotonic time
            /// deadline (in nanoseconds).
            fn set_oneshot_timer(deadline_ns: u64) {
                $crate::generic_timer::set_oneshot_timer(deadline_ns)
            }
        }
    };
}

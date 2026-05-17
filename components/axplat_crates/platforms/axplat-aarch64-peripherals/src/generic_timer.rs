//! ARM Generic Timer.
//!
//! Two timer modes coexist: CNTP (physical) and CNTV (virtual).
//! Default builds use CNTP, matching the QEMU TCG and physical-board
//! configurations everything else in the tree expects. The
//! `cntv-timer` feature switches to CNTV and IRQ 27 (PPI 11). This
//! is required under Apple HVF because the hypervisor owns CNTP at
//! EL2 and traps every EL1 access with `EC=Unknown`; CNTV is the
//! guest-owned timer that is always accessible from EL1.
//!
//! The timer-register choice is independent of the GIC version
//! (`gic-v3`). They are flipped together for the HVF profile by
//! platform crates that compose both features, but at this layer
//! the gates are orthogonal.

#[cfg(not(feature = "cntv-timer"))]
use aarch64_cpu::registers::{CNTFRQ_EL0, CNTP_TVAL_EL0, CNTPCT_EL0, Readable, Writeable};
#[cfg(feature = "cntv-timer")]
use aarch64_cpu::registers::{CNTFRQ_EL0, CNTV_TVAL_EL0, CNTVCT_EL0, Readable, Writeable};
use ax_int_ratio::Ratio;

// Named after the abstract "ticks" the trait exposes, not after a
// specific counter register, so the same names apply on both the
// CNTP and CNTV paths.
static mut TICKS_TO_NANOS_RATIO: Ratio = Ratio::zero();
static mut NANOS_TO_TICKS_RATIO: Ratio = Ratio::zero();

/// Returns the current clock time in hardware ticks.
#[inline]
pub fn current_ticks() -> u64 {
    #[cfg(feature = "cntv-timer")]
    {
        CNTVCT_EL0.get()
    }
    #[cfg(not(feature = "cntv-timer"))]
    {
        CNTPCT_EL0.get()
    }
}

/// Converts hardware ticks to nanoseconds.
#[inline]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    unsafe { TICKS_TO_NANOS_RATIO.mul_trunc(ticks) }
}

/// Converts nanoseconds to hardware ticks.
#[inline]
pub fn nanos_to_ticks(nanos: u64) -> u64 {
    unsafe { NANOS_TO_TICKS_RATIO.mul_trunc(nanos) }
}

/// Set a one-shot timer.
///
/// A timer interrupt will be triggered at the specified monotonic time deadline (in nanoseconds).
pub fn set_oneshot_timer(deadline_ns: u64) {
    let now = current_ticks();
    let deadline = nanos_to_ticks(deadline_ns);
    let interval = deadline.saturating_sub(now);
    debug_assert!(interval <= u32::MAX as u64);
    #[cfg(feature = "cntv-timer")]
    {
        CNTV_TVAL_EL0.set(interval);
    }
    #[cfg(not(feature = "cntv-timer"))]
    {
        CNTP_TVAL_EL0.set(interval);
    }
}

/// Early stage initialization: stores the timer frequency.
pub fn init_early() {
    let freq = CNTFRQ_EL0.get();
    unsafe {
        TICKS_TO_NANOS_RATIO = Ratio::new(ax_plat::time::NANOS_PER_SEC as u32, freq as u32);
        NANOS_TO_TICKS_RATIO = TICKS_TO_NANOS_RATIO.inverse();
    }
}

/// Enable timer interrupts.
///
/// It should be called on all CPUs, as the timer interrupt is a PPI (Private
/// Peripheral Interrupt).
#[cfg(feature = "irq")]
pub fn enable_irqs(timer_irq_num: usize) {
    #[cfg(feature = "cntv-timer")]
    {
        // CNTV (virtual timer) is the guest-owned timer at EL1. Used
        // when `cntv-timer` is enabled because Apple HVF traps CNTP
        // from EL1. CNTV PPI is 11 (IRQ 27); the matching axconfig
        // override sets `devices.timer-irq=27`.
        use aarch64_cpu::registers::CNTV_CTL_EL0;
        CNTV_CTL_EL0.write(CNTV_CTL_EL0::ENABLE::SET);
        CNTV_TVAL_EL0.set(0);
    }
    #[cfg(not(feature = "cntv-timer"))]
    {
        // CNTP (physical timer) is what every bare-metal / TCG
        // aarch64 board in tree expects. CNTP PPI is 14 (IRQ 30) on
        // the GIC distributor; existing axconfigs already advertise
        // 30 as `timer-irq`.
        use aarch64_cpu::registers::CNTP_CTL_EL0;
        CNTP_CTL_EL0.write(CNTP_CTL_EL0::ENABLE::SET);
        CNTP_TVAL_EL0.set(0);
    }
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

//! ARM Generic Timer.

use core::sync::atomic::{AtomicU64, Ordering};

const UNINIT_EPOCH_OFFSET_NANOS: u64 = u64::MAX;
static EPOCH_OFFSET_NANOS: AtomicU64 = AtomicU64::new(UNINIT_EPOCH_OFFSET_NANOS);

pub(crate) fn current_ticks() -> u64 {
    somehal::timer::ticks() as _
}

pub(crate) fn ticks_to_nanos(ticks: u64) -> u64 {
    let freq = somehal::timer::freq() as u64;
    if freq == 0 {
        return 0;
    }
    (ticks * ax_plat::time::NANOS_PER_SEC) / freq
}

pub(crate) fn nanos_to_ticks(nanos: u64) -> u64 {
    let freq = somehal::timer::freq() as u64;
    if freq == 0 {
        return 0;
    }
    (nanos * freq) / ax_plat::time::NANOS_PER_SEC
}

pub(crate) fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    let boot_offset = epoch_time_nanos.saturating_sub(ticks_to_nanos(current_ticks()));
    EPOCH_OFFSET_NANOS
        .compare_exchange(
            UNINIT_EPOCH_OFFSET_NANOS,
            boot_offset,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

struct GenericTimer;

#[impl_plat_interface]
impl ax_plat::time::TimeIf for GenericTimer {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        current_ticks()
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks_to_nanos(ticks)
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos_to_ticks(nanos)
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64 {
        match EPOCH_OFFSET_NANOS.load(Ordering::Acquire) {
            UNINIT_EPOCH_OFFSET_NANOS => 0,
            offset => offset,
        }
    }
    /// Returns the IRQ number for the timer interrupt.
    #[cfg(feature = "irq")]
    fn irq_num() -> usize {
        somehal::irq::systick_irq().into()
    }
    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds).
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64) {
        let cnptct = somehal::timer::ticks() as u64;
        let deadline = GenericTimer::nanos_to_ticks(deadline_ns);
        let interval = if cnptct < deadline {
            let interval = deadline - cnptct;
            debug_assert!(interval <= u32::MAX as u64);
            interval
        } else {
            0
        };

        somehal::timer::set_next_event_in_ticks(interval as _);
    }
}

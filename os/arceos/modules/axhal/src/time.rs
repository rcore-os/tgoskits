//! Time-related operations.

pub use ax_plat::time::{
    Duration, MICROS_PER_SEC, MILLIS_PER_SEC, NANOS_PER_MICROS, NANOS_PER_MILLIS, NANOS_PER_SEC,
    TimeValue, busy_wait, busy_wait_until, current_ticks, epochoffset_nanos, monotonic_time,
    monotonic_time_nanos, nanos_to_ticks, ticks_to_nanos, wall_time, wall_time_nanos,
};
#[cfg(feature = "irq")]
pub use ax_plat::time::{irq_num, set_oneshot_timer};

#[cfg(feature = "irq")]
pub fn enable_timer_irq() {
    #[cfg(any(test, feature = "host-test"))]
    {}

    #[cfg(not(any(test, feature = "host-test")))]
    crate::platform::enable_timer_irq();
}

pub fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    #[cfg(any(test, feature = "host-test"))]
    {
        let _ = epoch_time_nanos;
        false
    }

    #[cfg(not(any(test, feature = "host-test")))]
    crate::platform::try_init_epoch_offset(epoch_time_nanos)
}

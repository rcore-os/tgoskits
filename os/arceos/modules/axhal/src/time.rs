//! Time-related operations.

pub use ax_plat::time::{
    Duration, MICROS_PER_SEC, MILLIS_PER_SEC, NANOS_PER_MICROS, NANOS_PER_MILLIS, NANOS_PER_SEC,
    SchedulerClockError, SchedulerClockStability, TimeValue, busy_wait, busy_wait_until,
    current_ticks, epochoffset_nanos, init_scheduler_clock, monotonic_time, monotonic_time_nanos,
    nanos_to_ticks, scheduler_clock_hardirq_sample, scheduler_clock_source,
    scheduler_clock_stability, scheduler_clock_tick, shutdown_scheduler_clock, ticks_to_nanos,
    wall_time, wall_time_nanos,
};
#[cfg(feature = "irq")]
pub use ax_plat::time::{cancel_oneshot_timer, irq_num, resume_oneshot_timer, set_oneshot_timer};

pub fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    #[cfg(any(test, feature = "host-test"))]
    {
        let _ = epoch_time_nanos;
        false
    }

    #[cfg(not(any(test, feature = "host-test")))]
    crate::platform::try_init_epoch_offset(epoch_time_nanos)
}

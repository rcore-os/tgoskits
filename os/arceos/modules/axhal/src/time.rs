//! Time-related operations.

pub use ax_plat::time::{
    Duration, MICROS_PER_SEC, MILLIS_PER_SEC, NANOS_PER_MICROS, NANOS_PER_MILLIS, NANOS_PER_SEC,
    TimeValue, boot_elapsed_nanos, boot_elapsed_time, busy_wait, busy_wait_until, current_ticks,
    epochoffset_nanos, init_boot_time_base, monotonic_time, monotonic_time_nanos, nanos_to_ticks,
    ticks_to_nanos, wall_time, wall_time_nanos,
};
#[cfg(feature = "irq")]
pub use ax_plat::time::{irq_num, set_oneshot_timer};

pub fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    #[cfg(plat_dyn)]
    {
        axplat_dyn::try_init_epoch_offset(epoch_time_nanos)
    }
    #[cfg(not(plat_dyn))]
    {
        let _ = epoch_time_nanos;
        false
    }
}

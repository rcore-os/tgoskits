//! ArceOS implementations of the shared DRM kernel service boundaries.
//!
//! User memory, credentials and descriptors belong to the consuming process
//! layer and are installed by Starry, not by device discovery. These providers
//! only bind hardware-independent DRM mechanisms to the running task system.

use core::time::Duration;

use drm_kapi::clock::{ClockOps, install_clock_ops};

mod locks;
mod memory;
mod task;
mod vmap;
mod wait;

static CLOCK_OPS: ClockOps = ClockOps {
    now_ns: ax_hal::time::monotonic_time_nanos,
    delay_us,
    sleep_us,
    relax,
};

fn delay_us(micros: u32) {
    ax_hal::time::busy_wait(Duration::from_micros(u64::from(micros)));
}

fn sleep_us(micros: u64) {
    ax_task::thread::current::sleep(Duration::from_micros(micros));
}

fn relax() {
    if ax_task::thread::current::validate_blocking_context().is_ok() {
        // Relaxation is advisory; a scheduler transition may make yielding
        // unavailable after the context check without invalidating the poll.
        let _ = ax_task::thread::current::yield_current_cpu();
    } else {
        // Polling while an IRQ/preemption guard is held must not schedule.
        core::hint::spin_loop();
    }
}

/// Bind the shared mechanisms before the first DRM device is registered.
///
/// Reinstalling these same static providers is harmless. A competing provider
/// indicates an inconsistent runtime configuration and is rejected.
pub fn init() -> Result<(), i32> {
    install_clock_ops(&CLOCK_OPS).map_err(|_| -drm_kapi::config::EINVAL)?;
    drm_kapi::locks::install_lock_context_ops(&locks::OPS)
        .map_err(|_| -drm_kapi::config::EINVAL)?;
    drm_kapi::shmem::install_memory_address_ops(&memory::OPS)
        .map_err(|_| -drm_kapi::config::EINVAL)?;
    drm_kapi::vm::install_vmap_ops(&vmap::OPS).map_err(|_| -drm_kapi::config::EINVAL)?;
    drm_kapi::task_runtime::install_task_runtime_ops(&task::OPS)
        .map_err(|_| -drm_kapi::config::EINVAL)?;
    drm_kapi::wait::install_wait_ops(&wait::OPS).map_err(|_| -drm_kapi::config::EINVAL)?;
    wait::init()?;
    drm_kapi::timer::init_timer_executor().map_err(|_| -drm_kapi::config::EAGAIN)?;
    drm_kapi::workqueue::init_workqueue_executor().map_err(|_| -drm_kapi::config::EAGAIN)
}

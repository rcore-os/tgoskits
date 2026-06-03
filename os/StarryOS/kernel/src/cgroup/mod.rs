//! cgroup v2 subsystem skeleton.

mod core;
pub mod cpu;
pub mod pids;

pub use core::{CgroupNode, GLOBAL_CGROUP_ROOT};

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    core::init();
    ax_task::set_tick_hook(cpu::bandwidth_tick);
    info!("cgroup: initialized");
}

//! cgroup v2 subsystem skeleton.

mod core;
pub mod cpu;
pub mod pids;

pub use core::{CgroupNode, GLOBAL_CGROUP_ROOT};

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    core::init();
    // TODO: bandwidth_tick() requires ax_task::set_tick_hook which is deferred
    // ax_task::set_tick_hook(cpu::bandwidth_tick);
    info!("cgroup: initialized");
}

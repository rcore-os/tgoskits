//! cgroup v2 subsystem — kernel integration layer.
//!
//! Core logic lives in `ax-cgroup`. This module provides:
//! - Kernel-side `CgroupProvider` implementation
//! - `bandwidth_tick` hook (needs `ax_task` / `ax_hal`)
//! - Re-exports for backward compatibility

use alloc::sync::Arc;

pub use ax_cgroup::{
    CgroupId, CgroupNode, GLOBAL_CGROUP_ROOT, all_attr_names, attach_initial_process,
    attr_is_read_only, begin_fork, child_names, controllers_text, create_child, ensure_node_exists,
    events_text, exit_process, is_controller_attr, is_interface_file_name, lookup_child, path,
    procs_text, read_attr_at, register_provider, remove_child, root_id, stat_text,
    subtree_control_text, write_attr, write_procs, write_subtree_control,
};

mod cpu;

struct KernelCgroupProvider;

impl ax_cgroup::CgroupProvider for KernelCgroupProvider {
    fn is_zombie(&self, pid: u32) -> bool {
        crate::task::get_process_data(pid as _)
            .map(|pd| pd.proc.is_zombie())
            .unwrap_or(true)
    }

    fn get_cgroup(&self, pid: u32) -> Option<Arc<CgroupNode>> {
        crate::task::get_process_data(pid as _)
            .ok()
            .map(|pd| pd.cgroup.read().clone())
    }

    fn set_cgroup(&self, pid: u32, cgroup: Arc<CgroupNode>) {
        if let Ok(pd) = crate::task::get_process_data(pid as _) {
            *pd.cgroup.write() = cgroup;
        }
    }

    fn current_uid(&self) -> u32 {
        // Effective UID of the task performing the cgroupfs write, for
        // delegation checks. Kernel tasks (no thread) act as root (0).
        use crate::task::AsThread;
        ax_task::current()
            .try_as_thread()
            .map_or(0, |thr| thr.cred().euid)
    }

    fn notify_populated_changed(&self, cgroup_path: &str) {
        // cgroup2 is mounted both at the native `/cgroup` and at the
        // systemd-canonical `/sys/fs/cgroup`. inotify matches the watched path
        // exactly, so emit IN_MODIFY on the `cgroup.events` file under each
        // known mount point. A trailing slash on the root path is normalized
        // away so we never produce `//cgroup.events`.
        let rel = cgroup_path.strip_suffix('/').unwrap_or(cgroup_path);
        for mount in ["/cgroup", "/sys/fs/cgroup"] {
            let path = if rel.is_empty() || rel == "/" {
                alloc::format!("{mount}/cgroup.events")
            } else {
                alloc::format!("{mount}{rel}/cgroup.events")
            };
            crate::file::inotify::notify_modify_path(&path);
        }
    }
}

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    ax_cgroup::init();
    register_provider(&KernelCgroupProvider as &'static dyn ax_cgroup::CgroupProvider);
    // Drive cpu.max bandwidth throttling from the scheduler timer tick.
    ax_task::set_tick_hook(cpu::bandwidth_tick);
    info!("cgroup: initialized");
}

//! cgroup v2 subsystem — kernel integration layer.
//!
//! Core logic lives in `ax-cgroup`. This module provides:
//! - Kernel-side `CgroupProvider` implementation
//! - `bandwidth_tick` hook (needs `ax_task` / `ax_hal`)
//! - Re-exports for backward compatibility

use alloc::sync::Arc;

pub use ax_cgroup::{
    CgroupError, CgroupId, CgroupNode, GLOBAL_CGROUP_ROOT, all_attr_names, attach_initial_process,
    attr_is_read_only, begin_fork, child_names, controllers_text, create_child, ensure_node_exists,
    exit_process, is_controller_attr, is_interface_file_name, lookup_child, path, procs_text,
    read_attr_at, register_provider, remove_child, root_id, subtree_control_text, write_attr,
    write_procs, write_subtree_control,
};
use ax_errno::LinuxError;

/// Convert cgroup core error to VFS error.
/// Free function to satisfy Rust orphan rule: neither `CgroupError` nor
/// `VfsError` (= `AxError`) is defined in this crate.
pub(crate) fn cgroup_err_to_vfs(e: CgroupError) -> axfs_ng_vfs::VfsError {
    // Map to a Linux errno, then canonicalize to the Ax-native `AxErrorKind`
    // representation. Other pseudo-filesystems (proc/sysfs/overlay) return
    // Ax-native errors directly; VFS fast-paths compare against Ax-native
    // variants by value (e.g. `open(O_CREAT)`'s pre-resolve in fd_ops matches
    // `Err(AxError::NotFound)`). A Linux-tagged ENOENT is a distinct i32 from
    // the Ax-native NotFound, so without canonicalize() those matches miss and
    // a create on cgroupfs leaks ENOENT instead of reaching create() (EPERM).
    // The mapping is bijective for these codes, so the user-visible errno at
    // the syscall boundary is unchanged.
    let err: axfs_ng_vfs::VfsError = match e {
        CgroupError::NotInitialized => LinuxError::EINVAL.into(),
        CgroupError::NotFound => LinuxError::ENOENT.into(),
        CgroupError::AlreadyExists => LinuxError::EEXIST.into(),
        CgroupError::ResourceBusy => LinuxError::EBUSY.into(),
        CgroupError::InvalidInput => LinuxError::EINVAL.into(),
        CgroupError::NoSuchProcess => LinuxError::ESRCH.into(),
        CgroupError::OperationNotPermitted => LinuxError::EPERM.into(),
        CgroupError::DirectoryNotEmpty => LinuxError::ENOTEMPTY.into(),
        CgroupError::LimitExceeded => LinuxError::EAGAIN.into(),
    };
    err.canonicalize()
}

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
}

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    ax_cgroup::init();
    register_provider(&KernelCgroupProvider as &'static dyn ax_cgroup::CgroupProvider);

    // NOTE: CPU bandwidth tick hook deferred — ax_task::set_tick_hook and
    // set_throttled APIs are not yet available on dev branch.
    // ax_task::set_tick_hook(bandwidth_tick);

    info!("cgroup: initialized");
}

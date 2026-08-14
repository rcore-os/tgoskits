//! Kernel integration for the reusable cgroup domain model.

use alloc::{string::String, sync::Arc};
use core::fmt::Write;

use ax_cgroup::CgroupNode;
pub use ax_cgroup::{attach_initial_process, begin_fork, exit_process, relative_path, root};

use crate::{Errno, StarryError};

const INTERFACE_FILES: [&str; 3] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
];

struct KernelCgroupProvider;

impl ax_cgroup::CgroupProvider for KernelCgroupProvider {
    fn is_zombie(&self, pid: u32) -> bool {
        crate::task::is_zombie_pid(pid as _)
    }

    fn membership(&self, pid: u32) -> Option<Arc<CgroupNode>> {
        crate::task::get_process_data(pid as _)
            .ok()
            .map(|process| process.cgroup.read().clone())
    }

    fn set_membership(&self, pid: u32, cgroup: Arc<CgroupNode>) {
        if let Ok(process) = crate::task::get_process_data(pid as _) {
            *process.cgroup.write() = cgroup;
        }
    }
}

/// Initialize the cgroup hierarchy and kernel process provider.
pub fn init() {
    ax_cgroup::init();
    ax_cgroup::register_provider(&KernelCgroupProvider);
}

pub fn is_interface_file_name(name: &str) -> bool {
    INTERFACE_FILES.contains(&name)
}

pub fn controllers_text(_node: &CgroupNode) -> &'static str {
    ""
}

pub fn procs_text(node: &CgroupNode) -> String {
    let mut text = String::new();
    for pid in node.members() {
        let _ = writeln!(text, "{pid}");
    }
    text
}

pub fn subtree_control_text(_node: &CgroupNode) -> &'static str {
    ""
}

pub fn write_procs(node: Arc<CgroupNode>, data: &[u8]) -> Result<(), StarryError> {
    let pid = core::str::from_utf8(data)
        .map_err(|_| StarryError::InvalidInput)?
        .trim()
        .parse::<u32>()
        .map_err(|_| StarryError::InvalidInput)?;
    ax_cgroup::migrate_process(pid, node)?;
    Ok(())
}

pub fn write_subtree_control(_node: &CgroupNode, _data: &[u8]) -> Result<(), StarryError> {
    Err(Errno::EINVAL.into())
}

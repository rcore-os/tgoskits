//! Kernel integration for the reusable cgroup domain model.

use alloc::{string::String, sync::Arc};
use core::fmt::Write;

use ax_cgroup::{CgroupError, CgroupNode};
pub use ax_cgroup::{attach_initial_process, begin_fork, relative_path, root};
use ax_errno::{AxError, LinuxError};

use crate::task::{ProcessData, get_process_data};

const INTERFACE_FILES: [&str; 3] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
];

/// Initialize the cgroup hierarchy.
pub fn init() {
    ax_cgroup::init();
}

/// Move one live process under its generation-specific membership transaction.
pub fn migrate_process(pid: u32, target: Arc<CgroupNode>) -> Result<(), CgroupError> {
    let process = get_process_data(pid as _).map_err(|_| CgroupError::NoSuchProcess)?;
    process.migrate_cgroup(target)
}

/// Release membership without consulting the global PID registry.
pub fn exit_process(process: &ProcessData) {
    process.exit_cgroup();
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

pub fn write_procs(node: Arc<CgroupNode>, data: &[u8]) -> Result<(), AxError> {
    let pid = core::str::from_utf8(data)
        .map_err(|_| AxError::InvalidInput)?
        .trim()
        .parse::<u32>()
        .map_err(|_| AxError::InvalidInput)?;
    migrate_process(pid, node).map_err(cgroup_error)
}

pub fn write_subtree_control(_node: &CgroupNode, _data: &[u8]) -> Result<(), AxError> {
    Err(LinuxError::EINVAL.into())
}

pub fn cgroup_error(error: CgroupError) -> AxError {
    let error: AxError = match error {
        CgroupError::NotInitialized | CgroupError::InvalidInput => LinuxError::EINVAL.into(),
        CgroupError::NotFound => LinuxError::ENOENT.into(),
        CgroupError::AlreadyExists => LinuxError::EEXIST.into(),
        CgroupError::ResourceBusy => LinuxError::EBUSY.into(),
        CgroupError::NoSuchProcess => LinuxError::ESRCH.into(),
        CgroupError::DirectoryNotEmpty => LinuxError::ENOTEMPTY.into(),
    };
    error.canonicalize()
}

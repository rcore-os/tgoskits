use alloc::{string::String, vec::Vec};
use core::fmt::Write;

use ax_errno::{AxError, AxResult, LinuxError};

/// Returns the process list for the root cgroup.
pub fn root_procs_text() -> String {
    let mut pids: Vec<_> = crate::task::processes()
        .into_iter()
        .map(|proc_data| proc_data.proc.pid())
        .collect();
    pids.sort_unstable();

    let mut text = String::new();
    for pid in pids {
        let _ = writeln!(text, "{pid}");
    }
    text
}

/// Returns the controllers visible at the root cgroup.
pub fn root_controllers_text() -> &'static str {
    ""
}

/// Returns enabled subtree controllers at the root cgroup.
pub fn root_subtree_control_text() -> &'static str {
    ""
}

/// Writes to cgroup.procs are not implemented in this slice.
pub fn write_root_procs(_data: &[u8]) -> AxResult<()> {
    Err(AxError::from(LinuxError::EOPNOTSUPP))
}

/// Writes to cgroup.subtree_control cannot enable unavailable controllers.
pub fn write_root_subtree_control(_data: &[u8]) -> AxResult<()> {
    Err(AxError::from(LinuxError::EINVAL))
}

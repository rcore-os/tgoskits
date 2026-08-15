//! Kernel integration for the reusable cgroup domain model.

use alloc::{string::String, sync::Arc};
use core::fmt::Write;

use ax_cgroup::{CgroupError, CgroupForkGuard, CgroupNode, ProcessId};
pub use ax_cgroup::{relative_path, root};

use crate::{
    StarryError,
    task::{PidIdentity, PidIdentityId, PidView, ProcessData, Tgid, TgidNumber, UserTaskRef},
};

const INTERFACE_FILES: [&str; 3] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
];

fn process_id(identity: &PidIdentity) -> ProcessId {
    ProcessId::new(identity.id().get()).expect("PID identity generation must be non-zero")
}

fn process_identity(process: ProcessId) -> Option<Arc<PidIdentity>> {
    let identity_id = PidIdentityId::try_from(process.get()).ok()?;
    crate::task::ROOT_PID_NS.lookup_identity(identity_id)
}

/// Attach the first userspace process by stable PID generation.
pub fn attach_initial_process(identity: &Arc<PidIdentity>) -> Result<(), CgroupError> {
    ax_cgroup::attach_initial_process(process_id(identity))
}

/// Prepare inherited membership for a child process generation.
pub fn begin_fork(
    parent: Arc<CgroupNode>,
    child: &Arc<PidIdentity>,
) -> Result<CgroupForkGuard, CgroupError> {
    ax_cgroup::begin_fork(parent, process_id(child))
}

/// Initialize the cgroup hierarchy.
pub fn init() {
    ax_cgroup::init();
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

pub fn procs_text(current: &UserTaskRef, node: &CgroupNode) -> String {
    let mut text = String::new();
    let view = PidView::new(current.as_thread().active_pid_namespace());
    for process in node.members() {
        let Some(identity) = process_identity(process) else {
            continue;
        };
        if !identity.has_role::<Tgid>() {
            continue;
        }
        let Some(tgid) = view.visible_process_number(&identity) else {
            continue;
        };
        let _ = writeln!(text, "{tgid}");
    }
    text
}

pub fn subtree_control_text(_node: &CgroupNode) -> &'static str {
    ""
}

pub fn write_procs(
    current: &UserTaskRef,
    node: Arc<CgroupNode>,
    data: &[u8],
) -> Result<(), crate::StarryError> {
    let local_pid = core::str::from_utf8(data)
        .map_err(|_| crate::StarryError::InvalidInput)?
        .trim()
        .parse::<u32>()
        .map_err(|_| StarryError::InvalidInput)?;
    let identity = if local_pid == 0 {
        current.as_thread().proc_data.identity()
    } else {
        PidView::new(current.as_thread().active_pid_namespace())
            .resolve_process(TgidNumber::try_from(local_pid)?)?
    };
    identity
        .live_data()
        .ok_or(StarryError::NoSuchProcess)?
        .migrate_cgroup(node)
        .map_err(StarryError::from)
}

pub fn write_subtree_control(_node: &CgroupNode, _data: &[u8]) -> Result<(), crate::StarryError> {
    Err(crate::Errno::EINVAL.into())
}

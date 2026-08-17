//! Kernel integration for the reusable cgroup domain model.

use alloc::{string::String, sync::Arc};
use core::fmt::Write;

use ax_cgroup::{CgroupError, CgroupForkGuard, CgroupNode, ProcessId};
pub use ax_cgroup::{relative_path, root};
use ax_task::current;

use crate::{
    Errno, StarryError,
    task::{AsThread, PidIdentity, PidIdentityId, Tgid, TgidNumber, current_pid_view},
};

const INTERFACE_FILES: [&str; 3] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
];

struct KernelCgroupProvider;

impl ax_cgroup::CgroupProvider for KernelCgroupProvider {
    fn is_zombie(&self, process: ProcessId) -> bool {
        process_identity(process).is_some_and(|identity| identity.is_zombie())
    }

    fn membership(&self, process: ProcessId) -> Option<Arc<CgroupNode>> {
        process_identity(process)
            .and_then(|identity| identity.live_data())
            .map(|process| process.cgroup.read().clone())
    }

    fn set_membership(&self, process: ProcessId, cgroup: Arc<CgroupNode>) {
        if let Some(process) = process_identity(process).and_then(|identity| identity.live_data()) {
            *process.cgroup.write() = cgroup;
        }
    }
}

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

/// Release membership for one exact process generation.
pub fn exit_process(identity: &Arc<PidIdentity>) -> Result<(), CgroupError> {
    ax_cgroup::exit_process(process_id(identity))
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
    let view = current_pid_view();
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

pub fn write_procs(node: Arc<CgroupNode>, data: &[u8]) -> Result<(), StarryError> {
    let pid = core::str::from_utf8(data)
        .map_err(|_| StarryError::InvalidInput)?
        .trim()
        .parse::<u32>()
        .map_err(|_| StarryError::InvalidInput)?;
    let identity = if pid == 0 {
        current().as_thread().proc_data.identity()
    } else {
        crate::task::resolve_user_process_identity_by_number(TgidNumber::try_from(pid)?)?
    };
    ax_cgroup::migrate_process(process_id(&identity), node).map_err(StarryError::from)
}

pub fn write_subtree_control(_node: &CgroupNode, _data: &[u8]) -> Result<(), StarryError> {
    Err(Errno::EINVAL.into())
}

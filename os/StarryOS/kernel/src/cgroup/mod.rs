//! Kernel integration for the reusable cgroup domain model.

use alloc::{string::String, sync::Arc};
use core::fmt::Write;

use ax_cgroup::{
    CgroupChildKind, CgroupError, CgroupForkGuard, CgroupNode, CgroupTaskExit, ProcessId,
};
pub use ax_cgroup::{relative_path, root};
use ax_task::current;

use crate::{
    StarryError,
    task::{AsThread, PidIdentity, PidIdentityId, Tgid, TgidNumber, current_pid_view},
};

const INTERFACE_FILES: [&str; 7] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
    "pids.max",
    "pids.current",
    "pids.peak",
    "pids.events",
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

/// Reserve one task charge before publishing a child identity.
pub fn begin_task(
    process: &Arc<PidIdentity>,
    child: &Arc<PidIdentity>,
    child_kind: CgroupChildKind,
) -> Result<CgroupForkGuard, CgroupError> {
    ax_cgroup::begin_task(process_id(process), process_id(child), child_kind)
}

/// Reserve a process task in an explicit target cgroup.
pub fn begin_process_at(
    target: Arc<CgroupNode>,
    child: &Arc<PidIdentity>,
) -> Result<CgroupForkGuard, CgroupError> {
    ax_cgroup::begin_process_at(target, process_id(child))
}

/// Release one exact task generation and optionally its process membership.
pub fn exit_task(
    process: &Arc<PidIdentity>,
    task: &Arc<PidIdentity>,
    exit_kind: CgroupTaskExit,
) -> Result<(), CgroupError> {
    ax_cgroup::exit_task(process_id(process), process_id(task), exit_kind)
}

/// Rename one exact task generation after execve de-threading.
pub fn rename_task(
    process: &Arc<PidIdentity>,
    old_task: &Arc<PidIdentity>,
    new_task: &Arc<PidIdentity>,
) -> Result<(), CgroupError> {
    ax_cgroup::rename_task(
        process_id(process),
        process_id(old_task),
        process_id(new_task),
    )
}

/// Initialize the cgroup hierarchy and kernel process provider.
pub fn init() {
    ax_cgroup::init();
    ax_cgroup::register_provider(&KernelCgroupProvider);
}

pub fn is_interface_file_name(name: &str) -> bool {
    INTERFACE_FILES.contains(&name)
}

pub fn controllers_text(node: &CgroupNode) -> String {
    let mut text = String::new();
    for controller in node.available_controllers() {
        let _ = writeln!(text, "{controller}");
    }
    text
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

pub fn subtree_control_text(node: &CgroupNode) -> String {
    let mut text = String::new();
    for controller in node.enabled_subtree_controllers() {
        let _ = writeln!(text, "{controller}");
    }
    text
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

pub fn write_subtree_control(node: &CgroupNode, data: &[u8]) -> Result<(), StarryError> {
    let data = core::str::from_utf8(data).map_err(|_| StarryError::InvalidInput)?;
    node.write_subtree_control(data).map_err(StarryError::from)
}

pub fn pids_max_text(node: &CgroupNode) -> Result<String, StarryError> {
    node.pids_max_text().map_err(StarryError::from)
}

pub fn pids_current_text(node: &CgroupNode) -> Result<String, StarryError> {
    node.pids_current_text().map_err(StarryError::from)
}

pub fn pids_peak_text(node: &CgroupNode) -> Result<String, StarryError> {
    node.pids_peak_text().map_err(StarryError::from)
}

pub fn pids_events_text(node: &CgroupNode) -> Result<String, StarryError> {
    node.pids_events_text().map_err(StarryError::from)
}

pub fn write_pids_max(node: &CgroupNode, data: &[u8]) -> Result<(), StarryError> {
    let data = core::str::from_utf8(data).map_err(|_| StarryError::InvalidInput)?;
    node.write_pids_max(data).map_err(StarryError::from)
}

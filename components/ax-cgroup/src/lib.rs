//! Cgroup v2 hierarchy, process membership, and namespace ownership.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod membership;
mod namespace;
mod node;
mod pids;
mod sync;

use alloc::sync::Arc;
use core::{fmt, num::NonZeroU64};

use ax_lazyinit::LazyInit;
pub use membership::{CgroupChildKind, CgroupForkGuard, CgroupTaskExit, ProcessMembership};
pub use namespace::CgroupNamespace;
pub use node::{CgroupNode, CgroupPin};

/// Stable, non-reusable process generation used by cgroup membership.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProcessId(NonZeroU64);

impl ProcessId {
    /// Construct a stable process generation from its non-zero kernel value.
    pub const fn new(id: u64) -> Option<Self> {
        match NonZeroU64::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    /// Return the stable kernel generation value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

/// Cgroup operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CgroupError {
    /// The cgroup subsystem is unavailable.
    #[error("cgroup subsystem is not initialized")]
    NotInitialized,
    /// The requested cgroup does not exist.
    #[error("cgroup was not found")]
    NotFound,
    /// A child cgroup with the requested name already exists.
    #[error("cgroup already exists")]
    AlreadyExists,
    /// The cgroup is still referenced, populated, or has a conflicting member.
    #[error("cgroup is busy")]
    ResourceBusy,
    /// A task creation would exceed a pids limit in this cgroup hierarchy.
    #[error("cgroup pids limit exceeded")]
    LimitExceeded,
    /// The supplied name, PID, or file content is invalid.
    #[error("invalid cgroup input")]
    InvalidInput,
    /// The requested process does not exist or is already a zombie.
    #[error("process does not exist")]
    NoSuchProcess,
    /// The cgroup still contains child cgroups.
    #[error("cgroup directory is not empty")]
    DirectoryNotEmpty,
}

/// Result returned by cgroup domain operations.
pub type CgroupResult<T> = Result<T, CgroupError>;

static ROOT: LazyInit<Arc<CgroupNode>> = LazyInit::new();

/// Initialize the global cgroup hierarchy.
pub fn init() {
    ROOT.init_once(CgroupNode::new_root());
    membership::init();
}

/// Return a strong handle to the global cgroup root.
pub fn root() -> Arc<CgroupNode> {
    ROOT.get()
        // SAFE-EXPECT: kernel startup initializes the cgroup subsystem before any process exists.
        .expect("cgroup subsystem must be initialized before use")
        .clone()
}

/// Attach the first userspace process to the global root.
pub fn attach_initial_process(pid: ProcessId) -> CgroupResult<()> {
    membership::attach_initial_process(root(), pid)
}

/// Reserve a non-thread child directly in an explicit target cgroup.
pub fn begin_process_at(
    target: Arc<CgroupNode>,
    child_pid: ProcessId,
) -> CgroupResult<CgroupForkGuard> {
    membership::begin_task_at(target, child_pid, child_pid, CgroupChildKind::Process)
}
/// Render `target` relative to an arbitrary cgroup namespace root.
pub fn relative_path(root: &Arc<CgroupNode>, target: &Arc<CgroupNode>) -> alloc::string::String {
    node::relative_path(root, target)
}

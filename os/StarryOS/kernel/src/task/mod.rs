//! User task management.

mod bounded_stack;
mod cred;
pub mod futex;
pub mod future;
mod interruption;
mod job_control;
mod ops;
mod pid_namespace;
pub mod posix_timer;
mod process_accounting;
mod process_cgroup;
mod process_identity;
mod process_image;
mod process_memory;
mod process_policy;
mod process_ptrace;
mod process_wait;
mod resources;
mod scheduler_identity;
mod scheduler_task;
mod seccomp;
mod signal;
mod signal_publication;
mod stat;
mod thread;
mod tid;
mod timer;
mod user;
mod user_memory_access;
mod user_wait;

use alloc::sync::Arc;

use ax_sync::{PiMutex, spin::SpinNoIrq};
pub use process_ptrace::{PtraceStopFpData, SyscallTraceState};
use starry_process::{Pid, Process};
use starry_signal::{
    Signo,
    api::{ProcessSignalManager, SignalActions},
};

pub use self::{
    cred::*, futex::*, job_control::JobStatus, ops::*, posix_timer::PosixTimerTable,
    process_image::ProcessImage, process_wait::wait_on_pollset, resources::*, scheduler_task::*,
    seccomp::*, signal::*, stat::*, thread::Thread, tid::*, timer::*, user::*,
};
use self::{
    job_control::ProcessJobControl, process_accounting::ProcessAccountingState,
    process_cgroup::ProcessCgroupState, process_image::ProcessImageState,
    process_memory::ProcessMemoryState, process_policy::ProcessPolicyState,
    process_ptrace::ProcessPtraceState, process_wait::ProcessWaitState,
};
#[cfg(axtest)]
pub(crate) use self::{
    ops::decode_wait_status_rules_hold_for_test,
    posix_timer::posix_timer_clock_validation_rules_hold_for_test,
    seccomp::seccomp_action_and_precedence_rules_hold_for_test,
    seccomp::seccomp_bpf_constants_hold_for_test,
    timer::itimer_type_signo_and_time_conversion_rules_hold_for_test,
};
pub(crate) use self::{pid_namespace::*, process_identity::*};
use crate::mm::AddrSpace;

pub struct ProcessData {
    /// The process.
    pub proc: Arc<Process>,
    /// Stable generation identity shared by the registry and pidfds.
    identity: Arc<ProcessIdentity>,
    /// Executable metadata independently synchronized for exec and procfs.
    image: ProcessImageState,
    /// Address-space publication and release state.
    memory: ProcessMemoryState,
    /// The per-process uprobe manager. Each process has its own because user
    /// code can be modified independently.
    pub uprobe_manager: crate::kprobe::KprobeManager,
    /// Per-process uprobe point list, paired with [`Self::uprobe_manager`].
    pub uprobe_point_list: PiMutex<crate::kprobe::KprobePointList>,
    /// The namespace proxy — aggregates all namespace types for this process.
    pub nsproxy: SpinNoIrq<axnsproxy::NsProxy>,
    /// Authoritative cgroup membership and exit serialization.
    cgroup: ProcessCgroupState,
    /// Resource limits and process-wide compatibility policy.
    policy: ProcessPolicyState,

    /// Exit metadata, wait channels, and vfork completion.
    wait: ProcessWaitState,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager>,

    /// The futex table.
    futex_table: Arc<FutexTable>,

    /// CPU accounting and process-owned timer tables.
    accounting: ProcessAccountingState,

    /// Ptrace stop/resume state and architecture register snapshots.
    ptrace: ProcessPtraceState,

    /// Job-control stop state and parent-report delivery.
    job_control: ProcessJobControl,
}

/// Resources and Linux-visible metadata consumed by process construction.
pub struct ProcessDataInit {
    image: ProcessImage,
    aspace: Arc<PiMutex<AddrSpace>>,
    signal_actions: Arc<SpinNoIrq<SignalActions>>,
    nsproxy: axnsproxy::NsProxy,
    cgroup: Arc<ax_cgroup::CgroupNode>,
    exit_signal: Option<Signo>,
    wait_parent_tid: Pid,
    vm_aspace_shared: bool,
}

impl ProcessDataInit {
    /// Collects the resources that become owned by one process identity.
    pub fn new(
        image: ProcessImage,
        aspace: Arc<PiMutex<AddrSpace>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
        nsproxy: axnsproxy::NsProxy,
        exit_signal: Option<Signo>,
        wait_parent_tid: Pid,
        vm_aspace_shared: bool,
    ) -> Self {
        Self {
            image,
            aspace,
            signal_actions,
            nsproxy,
            cgroup: crate::cgroup::root(),
            exit_signal,
            wait_parent_tid,
            vm_aspace_shared,
        }
    }

    /// Selects inherited membership for a prepared non-thread child.
    pub fn with_cgroup(mut self, cgroup: Arc<ax_cgroup::CgroupNode>) -> Self {
        self.cgroup = cgroup;
        self
    }
}

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(proc: Arc<Process>, init: ProcessDataInit) -> Arc<Self> {
        let ProcessDataInit {
            image,
            aspace,
            signal_actions,
            nsproxy,
            cgroup,
            exit_signal,
            wait_parent_tid,
            vm_aspace_shared,
        } = init;
        let pid_namespaces: Arc<[axnsproxy::PidNamespaceRef]> =
            axnsproxy::pid_namespace_lineage(&nsproxy.pid_ns).into();
        let this = Arc::new_cyclic(|weak| {
            let wait = ProcessWaitState::new(exit_signal, wait_parent_tid);
            let identity = ProcessIdentity::new(
                proc.clone(),
                wait.exit_event_arc(),
                weak.clone(),
                pid_namespaces.clone(),
            );
            Self {
                proc,
                identity,
                image: ProcessImageState::new(image),
                memory: ProcessMemoryState::new(aspace, vm_aspace_shared),
                wait,
                uprobe_manager: crate::kprobe::KprobeManager::new(),
                uprobe_point_list: PiMutex::new(crate::kprobe::KprobePointList::new()),

                policy: ProcessPolicyState::new(),
                accounting: ProcessAccountingState::new(),

                signal: Arc::new(ProcessSignalManager::new(
                    signal_actions,
                    crate::config::SIGNAL_TRAMPOLINE,
                )),

                futex_table: Arc::new(FutexTable::new()),

                nsproxy: SpinNoIrq::new(nsproxy),
                cgroup: ProcessCgroupState::new(cgroup),

                ptrace: ProcessPtraceState::new(),

                job_control: ProcessJobControl::new(),
            }
        });
        // Clone the Arc in a separate statement: a temporary `SpinNoIrq` guard
        // from `lock()` lives until the end of the statement, so calling
        // `attach_process_slot` (which locks `PiMutex<AddrSpace>`) in the same
        // expression would nest a sleepable lock inside atomic context.
        let aspace_arc = this.aspace();
        crate::mm::attach_process_slot(&aspace_arc);
        this
    }

    /// Returns this process generation's stable PID identity.
    pub(crate) fn identity(&self) -> Arc<ProcessIdentity> {
        self.identity.clone()
    }

    /// Returns a stable snapshot of this process generation's current or final cgroup node.
    pub(crate) fn cgroup_node(&self) -> Arc<ax_cgroup::CgroupNode> {
        self.cgroup.current()
    }

    /// Moves this live process under its per-generation PI transaction.
    pub(crate) fn migrate_cgroup(
        &self,
        target: Arc<ax_cgroup::CgroupNode>,
    ) -> ax_cgroup::CgroupResult<()> {
        self.cgroup.migrate(self.proc.pid(), target)
    }

    /// Removes this generation from the hierarchy exactly once.
    pub(crate) fn exit_cgroup(&self) {
        self.cgroup.exit(self.proc.pid());
    }
}

impl Drop for ProcessData {
    fn drop(&mut self) {
        self.release_aspace_slot_if_needed();
    }
}

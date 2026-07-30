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

use ax_sync::{PiMutex, PiMutexGuard, spin::SpinNoIrq};
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
#[cfg(axtest)]
pub(crate) use self::{
    futex::empty_wake_op_entry_allocations_for_test,
    ops::decode_wait_status_rules_hold_for_test,
    posix_timer::{
        posix_timer_active_gate_rules_hold_for_test,
        posix_timer_clock_sampling_rules_hold_for_test,
        posix_timer_clock_validation_rules_hold_for_test,
        posix_timer_expiry_batch_rules_hold_for_test,
        posix_timer_saturating_timespec_rules_hold_for_test,
        posix_timer_stale_expiry_signal_is_suppressed_for_test,
    },
    seccomp::seccomp_action_and_precedence_rules_hold_for_test,
    seccomp::seccomp_bpf_constants_hold_for_test,
    timer::{
        alarm_generation_rules_hold_for_test, cpu_interval_timers_avoid_wall_alarms_for_test,
        interval_timer_arm_uses_current_snapshot_for_test,
        itimer_type_signo_and_time_conversion_rules_hold_for_test,
        scheduler_tick_accounting_excludes_state_writer_for_test,
        scheduler_tick_group_accounting_is_aggregate_for_test,
    },
};
use self::{
    job_control::ProcessJobControl, process_accounting::ProcessAccountingState,
    process_cgroup::ProcessCgroupState, process_image::ProcessImageState,
    process_memory::ProcessMemoryState, process_policy::ProcessPolicyState,
    process_ptrace::ProcessPtraceState, process_wait::ProcessWaitState,
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
    /// Short raw publication lock for the structurally immutable aggregate.
    ///
    /// Namespace objects referenced by the aggregate retain their own locks
    /// because processes in the same namespace intentionally share them.
    nsproxy: SpinNoIrq<Arc<axnsproxy::NsProxy>>,
    /// Sleepable writer transaction gate for process-wide namespace updates.
    namespace_update: PiMutex<()>,
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

                nsproxy: SpinNoIrq::new(Arc::new(nsproxy)),
                namespace_update: PiMutex::new(()),
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

    /// Returns a stable namespace aggregate without retaining the raw lock.
    pub(crate) fn namespace_snapshot(&self) -> Arc<axnsproxy::NsProxy> {
        self.nsproxy.lock().clone()
    }

    /// Serializes one process-wide namespace mutation or replacement.
    ///
    /// This is the outermost task-context lock for namespace changes. A
    /// transaction may acquire a thread scope or filesystem-context lock, but
    /// code holding either of those locks must not start a namespace update.
    pub(crate) fn namespace_update(&self) -> ProcessNamespaceUpdate<'_> {
        ProcessNamespaceUpdate {
            publication: &self.nsproxy,
            _guard: self.namespace_update.lock(),
        }
    }

    /// Takes one consistent namespace snapshot for a new process.
    ///
    /// A PID namespace staged by `unshare` or `setns` is consumed from the
    /// same published snapshot copied for the child. The caller must retain
    /// the returned namespace in a rollback guard until child publication.
    pub(crate) fn prepare_child_namespaces(
        &self,
    ) -> (axnsproxy::NsProxy, Option<axnsproxy::PidNamespaceRef>) {
        let update = self.namespace_update();
        let snapshot = update.snapshot();
        let child = snapshot.clone_all();
        let mut replacement = snapshot.clone_for_unshare();
        match replacement.child_pid_ns.take() {
            Some(namespace) => {
                update.publish(replacement);
                (child, Some(namespace))
            }
            None => (child, None),
        }
    }

    /// Releases the process-owned cgroup namespace after the final thread exits.
    pub(crate) fn release_cgroup_namespace(&self) {
        let update = self.namespace_update();
        let mut replacement = update.snapshot().clone_for_unshare();
        replacement.release_cgroup_namespace();
        update.publish(replacement);
    }
}

impl Drop for ProcessData {
    fn drop(&mut self) {
        self.release_aspace_slot_if_needed();
    }
}

/// A serialized namespace writer whose preparation happens outside the raw
/// publication lock.
///
/// The writer gate remains held while a caller mutates an object reachable
/// from the current snapshot. This prevents a concurrent replacement from
/// making that mutation invisible to the process.
pub(crate) struct ProcessNamespaceUpdate<'a> {
    publication: &'a SpinNoIrq<Arc<axnsproxy::NsProxy>>,
    _guard: PiMutexGuard<'a, ()>,
}

impl ProcessNamespaceUpdate<'_> {
    /// Returns the namespace snapshot on which this transaction should build.
    pub(crate) fn snapshot(&self) -> Arc<axnsproxy::NsProxy> {
        self.publication.lock().clone()
    }

    /// Publishes a fully prepared namespace set and releases old resources
    /// after both the raw publication lock and writer gate are released.
    pub(crate) fn publish(self, replacement: axnsproxy::NsProxy) {
        let replacement = Arc::new(replacement);
        let previous = {
            let mut current = self.publication.lock();
            core::mem::replace(&mut *current, replacement)
        };
        drop(self);
        drop(previous);
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessData;

    #[test]
    fn namespace_publication_lock_only_contains_a_shared_snapshot() {
        fn assert_snapshot_lock(
            _: &ax_sync::spin::SpinNoIrq<alloc::sync::Arc<axnsproxy::NsProxy>>,
        ) {
        }
        fn assert_process_lock_type(process: &ProcessData) {
            assert_snapshot_lock(&process.nsproxy);
        }

        let _ = assert_process_lock_type as fn(&ProcessData);
    }
}

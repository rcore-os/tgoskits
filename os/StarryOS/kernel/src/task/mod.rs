//! User task management.

mod bounded_stack;
mod cgroup_exit_invariant;
mod cred;
pub mod futex;
pub mod future;
mod interruption;
mod job_control;
mod ops;
mod pid;
pub mod posix_timer;
mod process;
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
mod timer;
#[cfg(target_arch = "loongarch64")]
mod unaligned;
mod user;
mod user_memory_access;
mod user_wait;

use alloc::sync::Arc;

pub(crate) use process_ptrace::PtraceAttachMode;
pub use process_ptrace::{PtraceStopFpData, SyscallTraceState};
use starry_signal::{
    Signo,
    api::{ProcessSignalManager, SignalActions},
};

pub use self::{
    cred::*, futex::*, job_control::JobStatus, ops::*, posix_timer::PosixTimerTable, process::*,
    process_image::ProcessImage, process_wait::wait_on_pollset, resources::*, scheduler_task::*,
    seccomp::*, signal::*, stat::*, thread::Thread, timer::*, user::*,
};
#[cfg(axtest)]
pub(crate) use self::{
    futex::futex_nofault_failure_is_transactional_for_test,
    pid::{
        dropped_exit_path_lease_keeps_unfinished_work_pending_for_test,
        exit_path_completion_precedes_task_transfer_for_test,
        pid_identity_state_machine_rules_hold_for_test,
    },
    posix_timer::{
        posix_timer_active_gate_rules_hold_for_test,
        posix_timer_clock_sampling_rules_hold_for_test,
        posix_timer_expiry_batch_rules_hold_for_test,
        posix_timer_saturating_timespec_rules_hold_for_test,
        posix_timer_stale_expiry_signal_is_suppressed_for_test,
    },
    process_cgroup::task_exit_transaction_holds_membership_lock_for_test,
    process_identity::reaped_process_handle_retains_exact_identity_for_test,
    process_ptrace::inactive_ptrace_syscall_gate_is_lock_free_for_test,
};
#[cfg(test)]
pub(crate) use self::{
    futex::{
        empty_wake_op_leaves_fixed_buckets_empty_for_test,
        futex_keys_follow_mm_and_backing_identity_for_test,
        queued_waiter_state_allocations_for_test,
    },
    ops::decode_wait_status_rules_hold_for_test,
    pid::pid_identity_state_machine_rules_hold_for_test,
    posix_timer::{
        posix_timer_active_gate_rules_hold_for_test,
        posix_timer_clock_sampling_rules_hold_for_test,
        posix_timer_clock_validation_rules_hold_for_test,
        posix_timer_expiry_batch_rules_hold_for_test,
        posix_timer_saturating_timespec_rules_hold_for_test,
        posix_timer_stale_expiry_signal_is_suppressed_for_test,
    },
    process_ptrace::inactive_ptrace_syscall_gate_is_lock_free_for_test,
    scheduler_task::{reset_yield_now_calls_for_test, yield_now_calls_for_test},
    seccomp::seccomp_action_and_precedence_rules_hold_for_test,
    seccomp::seccomp_bpf_constants_hold_for_test,
    timer::{
        alarm_generation_rules_hold_for_test, cpu_interval_timers_avoid_wall_alarms_for_test,
        interval_timer_arm_uses_current_snapshot_for_test,
        itimer_type_signo_and_time_conversion_rules_hold_for_test,
        scheduler_tick_group_accounting_is_aggregate_for_test,
        scheduler_tick_sampling_avoids_owner_writer_for_test,
        user_kernel_transitions_remain_task_local_for_test,
    },
};
use self::{
    job_control::ProcessJobControl, process_accounting::ProcessAccountingState,
    process_cgroup::ProcessCgroupState, process_image::ProcessImageState,
    process_memory::ProcessMemoryState, process_policy::ProcessPolicyState,
    process_ptrace::ProcessPtraceState, process_wait::ProcessWaitState,
};
pub(crate) use self::{pid::*, process_identity::*, process_memory::scheduler_address_space};
use crate::{
    mm::AddrSpace,
    namespace::NsProxy,
    sync::{IrqMutex, PiMutex, PiMutexGuard, SpinLock},
};

/// Resources shared by every thread in one Linux process generation.
pub struct ProcessData {
    /// Process topology object.
    pub proc: Arc<Process>,
    /// Stable identity shared by PID namespaces, pidfds, and observers.
    identity: Arc<PidIdentity>,
    /// TGID role ownership transferred into the zombie at final exit.
    tgid_lease: IrqMutex<Option<PidRoleLease<Tgid>>>,
    /// Executable metadata independently synchronized for exec and procfs.
    image: ProcessImageState,
    /// Address-space publication and release state.
    memory: ProcessMemoryState,
    /// Per-process uprobe manager.
    pub uprobe_manager: crate::kprobe::KprobeManager,
    /// Per-process uprobe point list.
    pub uprobe_point_list: PiMutex<crate::kprobe::KprobePointList>,
    /// Immutable namespace snapshot published under a short IRQ-safe lock.
    pub(crate) nsproxy: IrqMutex<Arc<NsProxy>>,
    /// Sleepable writer transaction gate for namespace replacement.
    namespace_update: PiMutex<()>,
    /// Authoritative cgroup membership and exit serialization.
    cgroup: ProcessCgroupState,
    /// Resource limits and process-wide compatibility policy.
    policy: ProcessPolicyState,
    /// Exit metadata, wait channels, and vfork completion.
    wait: ProcessWaitState,
    /// Process signal manager.
    pub signal: Arc<ProcessSignalManager>,
    /// CPU accounting and process-owned timer tables.
    accounting: ProcessAccountingState,
    /// Ptrace stop/resume state and architecture register snapshots.
    ptrace: ProcessPtraceState,
    /// Job-control stop state and parent-report delivery.
    job_control: ProcessJobControl,
}

/// Fallible resources prepared before publishing one process generation.
pub struct ProcessDataInit {
    image: ProcessImage,
    aspace: Arc<PiMutex<AddrSpace>>,
    signal_actions: Arc<SpinLock<SignalActions>>,
    nsproxy: NsProxy,
    cgroup: Arc<ax_cgroup::CgroupNode>,
    exit_signal: Option<Signo>,
    wait_parent_tid: TidNumber,
    shared_memory: Option<process_memory::ProcessMemoryShare>,
}

impl ProcessDataInit {
    /// Collects the resources that become owned by one process identity.
    pub fn new(
        image: ProcessImage,
        aspace: Arc<PiMutex<AddrSpace>>,
        signal_actions: Arc<SpinLock<SignalActions>>,
        nsproxy: NsProxy,
        exit_signal: Option<Signo>,
        wait_parent_tid: TidNumber,
    ) -> Self {
        Self {
            image,
            aspace,
            signal_actions,
            nsproxy,
            cgroup: crate::cgroup::root(),
            exit_signal,
            wait_parent_tid,
            shared_memory: None,
        }
    }

    /// Makes a non-thread clone share the parent's Linux mm generation.
    pub(crate) fn with_shared_memory(mut self, parent: &ProcessData) -> Self {
        self.shared_memory = Some(parent.memory_share());
        self
    }

    /// Selects inherited membership for a prepared non-thread child.
    pub fn with_cgroup(mut self, cgroup: Arc<ax_cgroup::CgroupNode>) -> Self {
        self.cgroup = cgroup;
        self
    }
}

impl ProcessData {
    /// Creates one process aggregate around the already-reserved PID identity.
    pub fn new(
        proc: Arc<Process>,
        identity: Arc<PidIdentity>,
        tgid_lease: PidRoleLease<Tgid>,
        init: ProcessDataInit,
    ) -> Arc<Self> {
        let ProcessDataInit {
            image,
            aspace,
            signal_actions,
            nsproxy,
            cgroup,
            exit_signal,
            wait_parent_tid,
            shared_memory,
        } = init;
        let wait = ProcessWaitState::new(exit_signal, wait_parent_tid);
        let exit_event = wait.exit_event_arc();
        let this = Arc::new(Self {
            proc: proc.clone(),
            identity: identity.clone(),
            tgid_lease: IrqMutex::new(Some(tgid_lease)),
            image: ProcessImageState::new(image),
            memory: ProcessMemoryState::new(aspace, shared_memory),
            wait,
            uprobe_manager: crate::kprobe::KprobeManager::new(),
            uprobe_point_list: PiMutex::new(crate::kprobe::KprobePointList::new()),
            policy: ProcessPolicyState::new(),
            accounting: ProcessAccountingState::new(),
            signal: Arc::new(ProcessSignalManager::new(
                signal_actions,
                crate::config::SIGNAL_TRAMPOLINE,
            )),
            nsproxy: IrqMutex::new(Arc::new(nsproxy)),
            namespace_update: PiMutex::new(()),
            cgroup: ProcessCgroupState::new(&identity, cgroup),
            ptrace: ProcessPtraceState::new(),
            job_control: ProcessJobControl::new(),
        });
        identity.bind_process(proc, exit_event, Arc::downgrade(&this));
        let aspace = this.aspace();
        crate::mm::attach_process_slot(&aspace);
        this
    }

    /// Returns this process generation's stable PID identity.
    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity.clone()
    }

    /// Transfers the process-owned TGID lease into its immutable zombie.
    pub(crate) fn take_tgid_lease(&self) -> PidRoleLease<Tgid> {
        self.tgid_lease
            .lock()
            .take()
            .expect("process TGID lease transferred twice")
    }

    /// Returns the current or final cgroup membership snapshot.
    pub(crate) fn cgroup_node(&self) -> Arc<ax_cgroup::CgroupNode> {
        self.cgroup.current()
    }

    /// Moves this live process under its per-generation transaction.
    pub(crate) fn migrate_cgroup(
        &self,
        target: Arc<ax_cgroup::CgroupNode>,
    ) -> ax_cgroup::CgroupResult<()> {
        self.cgroup.migrate(target)
    }

    /// Reserve one child task charge under this process transaction.
    pub(crate) fn begin_cgroup_task(
        &self,
        child: ax_cgroup::ProcessId,
        child_kind: ax_cgroup::CgroupChildKind,
    ) -> ax_cgroup::CgroupResult<ax_cgroup::CgroupForkGuard> {
        self.cgroup.begin_task(child, child_kind)
    }

    /// Retire one thread-group entry and its cgroup charge as one transaction.
    pub(crate) fn finish_thread_exit(
        &self,
        task: ax_cgroup::ProcessId,
        transition: impl FnOnce() -> ThreadExit,
    ) -> (ThreadExit, ax_cgroup::CgroupResult<()>) {
        self.cgroup.finish_thread_exit(task, transition)
    }

    /// Rename one exact task charge after Linux de-threading.
    pub(crate) fn rename_cgroup_task(
        &self,
        old_task: ax_cgroup::ProcessId,
        new_task: ax_cgroup::ProcessId,
    ) -> ax_cgroup::CgroupResult<()> {
        self.cgroup.rename_task(old_task, new_task)
    }

    /// Returns a stable namespace aggregate without retaining the raw lock.
    pub(crate) fn namespace_snapshot(&self) -> Arc<NsProxy> {
        self.nsproxy.lock().clone()
    }

    /// Serializes one process-wide namespace mutation or replacement.
    pub(crate) fn namespace_update(&self) -> ProcessNamespaceUpdate<'_> {
        ProcessNamespaceUpdate {
            publication: &self.nsproxy,
            _guard: self.namespace_update.lock(),
        }
    }

    /// Releases the process-owned cgroup namespace after final exit.
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

/// Serialized copy-on-write namespace publication.
pub(crate) struct ProcessNamespaceUpdate<'a> {
    publication: &'a IrqMutex<Arc<NsProxy>>,
    _guard: PiMutexGuard<'a, ()>,
}

impl ProcessNamespaceUpdate<'_> {
    pub(crate) fn snapshot(&self) -> Arc<NsProxy> {
        self.publication.lock().clone()
    }

    pub(crate) fn publish(self, replacement: NsProxy) {
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
    use crate::{namespace::NsProxy, sync::IrqMutex};

    #[test]
    fn namespace_publication_lock_only_contains_a_shared_snapshot() {
        fn assert_snapshot_lock(_: &IrqMutex<alloc::sync::Arc<NsProxy>>) {}
        fn assert_process_lock_type(process: &ProcessData) {
            assert_snapshot_lock(&process.nsproxy);
        }

        let _ = assert_process_lock_type as fn(&ProcessData);
    }
}

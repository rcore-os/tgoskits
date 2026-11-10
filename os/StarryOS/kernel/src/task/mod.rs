//! User task management.

mod allocation;
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
mod proc_access;
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
#[cfg(target_arch = "aarch64")]
mod user_cpu_features;
mod user_memory_access;
mod user_wait;

use alloc::sync::Arc;
use core::any::Any;

pub(crate) use process_ptrace::{PtraceAttachMode, PtraceWaitAction, PtraceWaitStop};
pub use process_ptrace::{PtraceStopFpData, SyscallTraceState};
use starry_signal::{
    Signo,
    api::{ProcessSignalManager, SignalActions},
};

#[cfg(target_arch = "aarch64")]
pub(crate) use self::process_accounting::PerfSchedulerTickLease;
pub use self::{
    cred::*, futex::*, job_control::JobStatus, ops::*, posix_timer::PosixTimerTable, process::*,
    process_image::ProcessImage, process_wait::wait_on_pollset, resources::*, scheduler_task::*,
    seccomp::*, signal::*, stat::*, thread::Thread, timer::*, user::*,
};
use self::{
    job_control::ProcessJobControl, process_accounting::ProcessAccountingState,
    process_cgroup::ProcessCgroupState, process_image::ProcessImageState,
    process_memory::ProcessMemoryState, process_policy::ProcessPolicyState,
    process_ptrace::ProcessPtraceState, process_wait::ProcessWaitState,
};
pub(crate) use self::{
    pid::*,
    proc_access::authorize_proc_read,
    process_identity::*,
    process_memory::{PreparedProcessMemory, scheduler_address_space},
};
use crate::{
    mm::MmHandle,
    namespace::NsProxy,
    sync::{Mutex, MutexGuard, RawSpinLock},
};

/// Resources shared by every thread in one Linux process generation.
pub struct ProcessData {
    /// TGID role ownership transferred into the zombie at final exit.
    /// Keep this before the identity pins: failed clone can leave deferred
    /// task reclamation as the last owner after PID reservation rollback.
    tgid_lease: RawSpinLock<Option<PidRoleLease<Tgid>>>,
    /// Process topology object.
    pub proc: Arc<Process>,
    /// Controlling terminal inherited by this process generation.
    ///
    /// Linux stores this in the process signal state rather than only in the
    /// session's terminal relationship. Keeping the binding here lets
    /// `TIOCNOTTY` detach one non-session-leader without detaching its peers.
    controlling_terminal: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
    /// Stable identity shared by PID namespaces, pidfds, and observers.
    identity: Arc<PidIdentity>,
    /// Executable metadata independently synchronized for exec and procfs.
    image: ProcessImageState,
    /// Address-space publication and release state.
    memory: ProcessMemoryState,
    /// Per-process uprobe manager.
    pub uprobe_manager: crate::kprobe::KprobeManager,
    /// Per-process uprobe point list.
    pub uprobe_point_list: Mutex<crate::kprobe::KprobePointList>,
    /// Immutable namespace snapshot published under a short IRQ-safe lock.
    pub(crate) nsproxy: RawSpinLock<Arc<NsProxy>>,
    /// Sleepable publication gate shared by namespace and credential writers.
    namespace_update: Mutex<()>,
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
    aspace: MmHandle,
    signal_actions: Arc<RawSpinLock<SignalActions>>,
    nsproxy: NsProxy,
    cgroup: Arc<ax_cgroup::CgroupNode>,
    exit_signal: Option<Signo>,
    wait_parent_tid: TidNumber,
    shared_memory: Option<process_memory::ProcessMemoryShare>,
    controlling_terminal: Option<Arc<dyn Any + Send + Sync>>,
}

impl ProcessDataInit {
    /// Collects the resources that become owned by one process identity.
    pub fn new(
        image: ProcessImage,
        aspace: MmHandle,
        signal_actions: Arc<RawSpinLock<SignalActions>>,
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
            controlling_terminal: None,
        }
    }

    /// Makes a non-thread clone share the parent's Linux mm generation.
    pub(crate) fn with_shared_memory(mut self, parent: &ProcessData) -> Self {
        self.shared_memory = Some(parent.memory_share());
        self
    }

    /// Inherits the parent's controlling terminal across a process fork.
    pub(crate) fn with_controlling_terminal(mut self, parent: &ProcessData) -> Self {
        self.controlling_terminal = parent.controlling_terminal();
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
            controlling_terminal,
        } = init;
        let wait = ProcessWaitState::new(exit_signal, wait_parent_tid);
        let exit_event = wait.exit_event_arc();
        let signal = ProcessSignalManager::new(
            signal_actions,
            crate::config::SIGNAL_TRAMPOLINE,
            proc.group_exit_state(),
        );
        let signal = if proc.pid().get() == 1 {
            signal.protect_global_init()
        } else {
            signal
        };
        let this = Arc::new(Self {
            proc: proc.clone(),
            controlling_terminal: Mutex::new(controlling_terminal),
            identity: identity.clone(),
            tgid_lease: RawSpinLock::new(Some(tgid_lease)),
            image: ProcessImageState::new(image),
            memory: ProcessMemoryState::new(aspace, shared_memory),
            wait,
            uprobe_manager: crate::kprobe::KprobeManager::new(),
            uprobe_point_list: Mutex::new(crate::kprobe::KprobePointList::new()),
            policy: ProcessPolicyState::new(),
            accounting: ProcessAccountingState::new(),
            signal: Arc::new(signal),
            nsproxy: RawSpinLock::new(Arc::new(nsproxy)),
            namespace_update: Mutex::new(()),
            cgroup: ProcessCgroupState::new(&identity, cgroup),
            ptrace: ProcessPtraceState::new(),
            job_control: ProcessJobControl::new(),
        });
        identity.bind_process(proc, exit_event, Arc::downgrade(&this));
        this
    }

    /// Returns this process generation's stable PID identity.
    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity.clone()
    }

    /// Returns the controlling terminal binding for this process generation.
    pub(crate) fn controlling_terminal(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.controlling_terminal.lock().clone()
    }

    /// Publishes a controlling terminal binding for this process generation.
    pub(crate) fn set_controlling_terminal(&self, terminal: Arc<dyn Any + Send + Sync>) {
        *self.controlling_terminal.lock() = Some(terminal);
    }

    /// Drops this process generation's controlling terminal.
    pub(crate) fn clear_controlling_terminal(&self) {
        *self.controlling_terminal.lock() = None;
    }

    /// Clears a binding only when it is still the process's controlling terminal.
    pub(crate) fn clear_controlling_terminal_if(
        &self,
        terminal: &Arc<dyn Any + Send + Sync>,
    ) -> bool {
        let mut current = self.controlling_terminal.lock();
        if current
            .as_ref()
            .is_some_and(|existing| Arc::ptr_eq(existing, terminal))
        {
            *current = None;
            true
        } else {
            false
        }
    }

    /// Transfers the process-owned TGID lease into its immutable zombie.
    pub(crate) fn take_tgid_lease(&self) -> PidRoleLease<Tgid> {
        self.tgid_lease
            .lock_irqsave()
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
        self.nsproxy.lock_irqsave().clone()
    }

    /// Serializes one process-wide namespace mutation or replacement.
    pub(crate) fn namespace_update(&self) -> ProcessNamespaceUpdate<'_> {
        ProcessNamespaceUpdate {
            owner: self,
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
        self.retire_mm_owner();
    }
}

/// Serialized copy-on-write namespace publication.
pub(crate) struct ProcessNamespaceUpdate<'a> {
    owner: &'a ProcessData,
    publication: &'a RawSpinLock<Arc<NsProxy>>,
    _guard: MutexGuard<'a, ()>,
}

impl ProcessNamespaceUpdate<'_> {
    pub(super) fn owns(&self, process: &ProcessData) -> bool {
        core::ptr::eq(self.owner, process)
    }

    /// Publishes credentials without recursively acquiring this writer gate.
    pub(crate) fn set_process_cred(&self, thread: &Thread, cred: Cred) {
        assert!(self.owns(&thread.proc_data));
        thread.set_cred_under(self, cred);
    }

    pub(crate) fn snapshot(&self) -> Arc<NsProxy> {
        self.publication.lock_irqsave().clone()
    }

    pub(crate) fn publish(self, replacement: NsProxy) {
        let old = self.snapshot();
        if !Arc::ptr_eq(&old.user_ns, &replacement.user_ns) {
            for tid in self.owner.proc.threads() {
                if let Ok(task) = ops::get_task_by_number(tid) {
                    let thread = task.as_thread();
                    if self.owns(&thread.proc_data)
                        && !proc_access::namespace_preserves_permitted(
                            &old.user_ns,
                            &replacement.user_ns,
                            thread.cred().euid,
                        )
                    {
                        self.owner.set_dumpable(0);
                        break;
                    }
                }
            }
        }
        let replacement = Arc::new(replacement);
        let previous = {
            let mut current = self.publication.lock_irqsave();
            core::mem::replace(&mut *current, replacement)
        };
        drop(self);
        drop(previous);
    }
}

#[cfg(axtest)]
pub(crate) fn new_test_process_data(
    identity: Arc<PidIdentity>,
    tgid: PidRoleLease<Tgid>,
) -> Arc<ProcessData> {
    let parent = TidNumber::from(identity.root_number());
    ProcessData::new(
        Process::new_for_axtest(identity.clone()),
        identity,
        tgid,
        ProcessDataInit::new(
            ProcessImage::new(
                Default::default(),
                Arc::default(),
                Arc::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ),
            MmHandle::from_arc(Arc::new(Mutex::new(
                crate::mm::AddrSpace::new_empty(0x10000.into(), 0x10000).unwrap(),
            )))
            .unwrap(),
            Arc::default(),
            NsProxy::new_root(),
            None,
            parent,
        ),
    )
}

/// Axtest-only observation of the context in which `Process::group` acquires
/// its sleeping lock.
///
/// A caller that invokes `Process::group` with local IRQs disabled can panic
/// in the PI slow path, but only once contention appears. A test parks an
/// armed [`ProbeWindow`] around one real call and inspects the recorded
/// context instead, which makes the violation deterministic. Production
/// builds compile this away.
#[cfg(all(test, axtest))]
pub(crate) mod process_group_probe {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static ARMED: AtomicBool = AtomicBool::new(false);
    static ACQUISITIONS: AtomicUsize = AtomicUsize::new(0);
    static ACQUISITIONS_WITH_IRQS_DISABLED: AtomicUsize = AtomicUsize::new(0);

    /// Records the context of one `Process::group` lock acquisition.
    pub(crate) fn observe_acquisition_for_test() {
        if !ARMED.load(Ordering::Relaxed) {
            return;
        }
        ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
        if !ax_hal::cpu::interrupt::irqs_enabled() {
            ACQUISITIONS_WITH_IRQS_DISABLED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One armed observation window over `Process::group` acquisitions.
    pub(crate) struct ProbeWindow {
        acquisitions: usize,
        acquisitions_with_irqs_disabled: usize,
    }

    /// Arms the probe for the calling thread's next observation window.
    pub(crate) fn arm_for_test() -> ProbeWindow {
        ARMED.store(true, Ordering::Release);
        ProbeWindow {
            acquisitions: ACQUISITIONS.load(Ordering::Acquire),
            acquisitions_with_irqs_disabled: ACQUISITIONS_WITH_IRQS_DISABLED
                .load(Ordering::Acquire),
        }
    }

    impl ProbeWindow {
        /// Disarms the probe and returns the acquisitions this window covered
        /// and how many of them ran with local IRQs disabled.
        pub(crate) fn finish_for_test(self) -> (usize, usize) {
            ARMED.store(false, Ordering::Release);
            (
                ACQUISITIONS.load(Ordering::Acquire) - self.acquisitions,
                ACQUISITIONS_WITH_IRQS_DISABLED.load(Ordering::Acquire)
                    - self.acquisitions_with_irqs_disabled,
            )
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::ProcessData;
    use crate::{namespace::NsProxy, sync::RawSpinLock};

    #[test]
    fn namespace_publication_lock_only_contains_a_shared_snapshot() {
        fn assert_snapshot_lock(_: &RawSpinLock<alloc::sync::Arc<NsProxy>>) {}
        fn assert_process_lock_type(process: &ProcessData) {
            assert_snapshot_lock(&process.nsproxy);
        }

        let _ = assert_process_lock_type as fn(&ProcessData);
    }
}

//! Thread-owned state and its synchronization boundaries.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use ax_runtime::hal::{cpu::uspace::UserContext, percpu::CpuPin};
use axpoll_set::PollSet;
use scope_local::{LocalItem, Scope, ScopeActivationError, ScopeCell, ScopeCellWriteGuard};
use starry_signal::{SignalSet, api::ThreadSignalManager};

use super::{
    CpuTimeAccounting, Cred, ExitPathLease, PidIdentity, PidNamespaceRef, PidRoleLease,
    ProcessData, ROOT_PID_NS, RttimeWatchdog, SeccompDecision, SeccompState, SeccompStateStore,
    SockFilter, Tid, TidNumber, TimerState, UserTaskRef,
    bounded_stack::BoundedStack,
    futex::ThreadWaitState,
    interruption::{InterruptSnapshot, InterruptState},
    ops,
    scheduler_identity::SchedulerIdentity,
    user_memory_access::{UserMemoryAccessDepth, UserMemoryAccessGuard},
};
use crate::sync::{IrqMutex, NoPreemptIrqSave, PiMutex};

const KRETPROBE_STACK_CAPACITY: usize = 16;

/// User-visible and scheduler-visible identities retained by one Linux thread.
struct ThreadIdentity {
    scheduler: SchedulerIdentity,
    nice: AtomicI32,
}

impl ThreadIdentity {
    fn new() -> Self {
        Self {
            scheduler: SchedulerIdentity::unbound(),
            nice: AtomicI32::new(0),
        }
    }
}

/// Stable Linux identity ownership retained independently from scheduler IDs.
struct ThreadPidOwnership {
    identity: Arc<PidIdentity>,
    tid_lease: Option<PidRoleLease<Tid>>,
}

/// Scope-local resources and their task-context serialization.
struct ThreadScope {
    cell: ScopeCell,
    access: PiMutex<()>,
}

impl ThreadScope {
    fn new(scope: Scope) -> Self {
        Self {
            cell: ScopeCell::from_scope(scope),
            access: PiMutex::new(()),
        }
    }

    fn with_current_mut<R>(&self, operation: impl FnOnce(&mut ScopeCellWriteGuard<'_>) -> R) -> R {
        let _access = self.access.lock();
        let _guard = NoPreemptIrqSave::new();
        // SAFETY: the combined guard pins this task to the CPU and prevents
        // local IRQ reentry for the bounded lease transition and callback.
        unsafe {
            ax_runtime::hal::percpu::with_cpu_pin(|pin| {
                self.cell.try_with_active_mut_pinned(pin, operation)
            })
            .expect("Starry scope mutation requires an installed CPU area")
            .expect("serialized current scope mutation lost its sole scheduler activation")
        }
    }

    fn clone_item<T>(&self, item: &LocalItem<T>) -> T
    where
        T: Clone + Send + Sync + 'static,
    {
        let _access = self.access.lock();
        let scope = self
            .cell
            .try_read()
            .expect("serialized Starry scope read found an active writer");
        item.scope_cell(&scope).clone()
    }

    unsafe fn activate_pinned(&self, pin: &CpuPin<'_>) {
        // SAFETY: the scheduler switch baton retains this object and pins the
        // CPU until the matching switch-out callback.
        match unsafe { self.cell.try_activate_pinned(pin) } {
            Ok(()) => {}
            Err(ScopeActivationError::AlreadyActive) => {
                panic!("Starry scheduler attempted to activate one thread on two CPUs")
            }
            Err(ScopeActivationError::ExclusiveLease) => {
                panic!("Starry scheduler activated a thread during exclusive scope mutation")
            }
        }
    }

    unsafe fn deactivate_pinned(&self, pin: &CpuPin<'_>) {
        // SAFETY: forwarded from the matching scheduler switch-out callback.
        unsafe { self.cell.deactivate_pinned(pin) };
    }
}

/// Runtime accounting that follows scheduler switch callbacks.
struct ThreadAccounting {
    cpu_time: CpuTimeAccounting,
    rttime: PiMutex<RttimeWatchdog>,
}

impl ThreadAccounting {
    fn new() -> Self {
        Self {
            cpu_time: CpuTimeAccounting::new(),
            rttime: PiMutex::new(RttimeWatchdog::new()),
        }
    }
}

/// Thread-exit, userspace restart, and interruptible-wait state.
struct ThreadLifecycle {
    clear_child_tid: AtomicUsize,
    robust_list_head: AtomicUsize,
    exit: Arc<AtomicBool>,
    exit_started: AtomicBool,
    interrupted: InterruptState,
    user_memory_access: UserMemoryAccessDepth,
    block_next_signal_check: NextSignalCheckBlock,
    exit_event: Arc<PollSet>,
    exit_request: OneShotFlag,
    deadline_overrun: OneShotFlag,
    rseq_area: AtomicUsize,
    rseq_signature: AtomicU32,
}

impl ThreadLifecycle {
    fn new() -> Self {
        Self {
            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),
            exit: Arc::new(AtomicBool::new(false)),
            exit_started: AtomicBool::new(false),
            interrupted: InterruptState::new(),
            user_memory_access: UserMemoryAccessDepth::new(),
            block_next_signal_check: NextSignalCheckBlock::new(),
            exit_event: Arc::default(),
            exit_request: OneShotFlag::new(),
            deadline_overrun: OneShotFlag::new(),
            rseq_area: AtomicUsize::new(0),
            rseq_signature: AtomicU32::new(0),
        }
    }
}

/// Signal queue and signalfd notification state.
struct ThreadSignals {
    manager: Arc<ThreadSignalManager>,
    signalfd_waker: PollSet,
    deferred_mask_restore: IrqMutex<Option<SignalSet>>,
}

impl ThreadSignals {
    fn new(
        tid: u32,
        process_signal: Arc<starry_signal::api::ProcessSignalManager>,
        signal_mask: SignalSet,
    ) -> Self {
        Self {
            manager: ThreadSignalManager::new_with_blocked(tid, process_signal, signal_mask),
            signalfd_waker: PollSet::new(),
            deferred_mask_restore: IrqMutex::new(None),
        }
    }
}

/// Credentials and one-way security policy owned by a thread.
struct ThreadSecurity {
    oom_score_adj: AtomicI32,
    pdeathsig: AtomicU32,
    no_new_privs: AtomicBool,
    seccomp: SeccompStateStore,
    cred: PiMutex<Arc<Cred>>,
    uid_map_written: AtomicBool,
    gid_map_written: AtomicBool,
    setgroups_deny: AtomicBool,
}

impl ThreadSecurity {
    fn new(parent_cred: Option<Arc<Cred>>) -> Self {
        Self {
            oom_score_adj: AtomicI32::new(200),
            pdeathsig: AtomicU32::new(0),
            no_new_privs: AtomicBool::new(false),
            seccomp: SeccompStateStore::new(),
            cred: PiMutex::new(parent_cred.unwrap_or_else(|| Arc::new(Cred::root()))),
            uid_map_written: AtomicBool::new(false),
            gid_map_written: AtomicBool::new(false),
            setgroups_deny: AtomicBool::new(false),
        }
    }
}

/// Probe, crash-dump, and PMU state observed from trap or scheduler context.
struct ThreadTrace {
    fault_dump_signo: AtomicU8,
    kretprobe_stack:
        IrqMutex<BoundedStack<kprobe::retprobe::RetprobeInstance, KRETPROBE_STACK_CAPACITY>>,
    #[cfg(target_arch = "aarch64")]
    perf: crate::perf::task_context::ThreadPerfContext,
}

impl ThreadTrace {
    fn new() -> Self {
        Self {
            fault_dump_signo: AtomicU8::new(0),
            kretprobe_stack: IrqMutex::new(BoundedStack::new()),
            #[cfg(target_arch = "aarch64")]
            perf: crate::perf::task_context::ThreadPerfContext::new(),
        }
    }
}

/// A coalescing publication consumed exactly once by its owner thread.
struct OneShotFlag {
    pending: AtomicBool,
    #[cfg(axtest)]
    consume_rmws: AtomicUsize,
}

impl OneShotFlag {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            #[cfg(axtest)]
            consume_rmws: AtomicUsize::new(0),
        }
    }

    fn publish(&self) {
        self.pending.store(true, Ordering::Release);
    }

    fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    fn consume(&self) -> bool {
        if !self.is_pending() {
            return false;
        }
        #[cfg(axtest)]
        self.consume_rmws.fetch_add(1, Ordering::Relaxed);
        self.pending.swap(false, Ordering::AcqRel)
    }

    #[cfg(axtest)]
    fn consume_rmw_count(&self) -> usize {
        self.consume_rmws.load(Ordering::Relaxed)
    }
}

/// A one-shot flag that suppresses exactly one signal check.
struct NextSignalCheckBlock(OneShotFlag);

impl NextSignalCheckBlock {
    const fn new() -> Self {
        Self(OneShotFlag::new())
    }

    fn block(&self) {
        self.0.publish();
    }

    fn unblock(&self) -> bool {
        self.0.consume()
    }
}

/// The Starry state attached to one generation-bearing scheduler thread.
pub struct Thread {
    identity: ThreadIdentity,
    pid: IrqMutex<ThreadPidOwnership>,

    /// The process data shared by all threads in the process.
    pub proc_data: Arc<ProcessData>,

    scope: ThreadScope,
    accounting: ThreadAccounting,
    lifecycle: ThreadLifecycle,
    wait: ThreadWaitState,
    signals: ThreadSignals,
    security: ThreadSecurity,
    trace: ThreadTrace,
}

impl Thread {
    /// Creates a new thread state object before the scheduler identity is bound.
    pub fn new(
        identity: Arc<PidIdentity>,
        tid_lease: PidRoleLease<Tid>,
        proc_data: Arc<ProcessData>,
        parent_cred: Option<Arc<Cred>>,
        signal_mask: SignalSet,
        scope: Scope,
    ) -> Box<Self> {
        let tid = identity
            .visible_number(&ROOT_PID_NS)
            .expect("new thread identity has no root PID binding")
            .get();
        let process_signal = proc_data.signal.clone();
        let process_identity = proc_data.identity();
        let thread = Box::new(Self {
            identity: ThreadIdentity::new(),
            pid: IrqMutex::new(ThreadPidOwnership {
                identity: identity.clone(),
                tid_lease: Some(tid_lease),
            }),
            proc_data,
            scope: ThreadScope::new(scope),
            accounting: ThreadAccounting::new(),
            lifecycle: ThreadLifecycle::new(),
            wait: ThreadWaitState::new(),
            signals: ThreadSignals::new(tid, process_signal, signal_mask),
            security: ThreadSecurity::new(parent_cred),
            trace: ThreadTrace::new(),
        });
        identity.bind_thread_pidfd(&process_identity, thread.exit_flag());
        thread
    }

    pub(super) const fn wait_state(&self) -> &ThreadWaitState {
        &self.wait
    }

    /// Mutates the current thread's resource scope.
    ///
    /// Contending task-context readers and writers sleep on the outer PI mutex.
    /// The closure itself runs with preemption and local IRQs disabled and must
    /// only install already-prepared scope entries.
    pub(crate) fn with_current_scope_mut<R>(
        &self,
        f: impl FnOnce(&mut ScopeCellWriteGuard<'_>) -> R,
    ) -> R {
        self.scope.with_current_mut(f)
    }

    /// Clones one owned scope-local value under task-context serialization.
    ///
    /// The scope lease and writer gate are released before the returned owner
    /// can acquire any lock it contains.
    pub(crate) fn clone_scope_item<T>(&self, item: &LocalItem<T>) -> T
    where
        T: Clone + Send + Sync + 'static,
    {
        self.scope.clone_item(item)
    }

    /// Returns the root-namespace TID, independent from the scheduler ID.
    pub fn tid(&self) -> TidNumber {
        self.tid_number()
    }

    pub(crate) fn tid_number(&self) -> TidNumber {
        TidNumber::from(
            self.pid
                .lock()
                .identity
                .visible_number(&ROOT_PID_NS)
                .expect("live thread lost its root PID binding"),
        )
    }

    /// Returns this thread's stable PID generation.
    pub(crate) fn pid_identity(&self) -> Arc<PidIdentity> {
        self.pid.lock().identity.clone()
    }

    /// Returns the active PID namespace derived from the identity itself.
    pub(crate) fn active_pid_namespace(&self) -> PidNamespaceRef {
        self.pid.lock().identity.active_namespace()
    }

    /// Returns the TID as observed from this thread's active namespace.
    pub(crate) fn user_tid(&self) -> TidNumber {
        let pid = self.pid.lock();
        let active = pid.identity.active_namespace();
        TidNumber::from(
            pid.identity
                .visible_number(&active)
                .expect("thread identity is not visible from its active namespace"),
        )
    }

    /// Publishes the runtime link immediately before scheduler activation.
    pub(crate) fn attach_pid_task(&self, task: &UserTaskRef) {
        self.pid.lock().identity.attach_task(task);
    }

    /// Releases the runtime link while transferring the TID role and the
    /// remaining exit-path responsibility to the caller.
    ///
    /// The returned [`ExitPathLease`] owns everything the retired task still
    /// owes its PID namespaces; the caller completes it once zombie
    /// publication, parent notification, and relation close finished.
    pub(crate) fn retire_pid_retaining_tid(&self) -> (PidRoleLease<Tid>, ExitPathLease) {
        let (identity, lease) = {
            let mut pid = self.pid.lock();
            (pid.identity.clone(), pid.tid_lease.take())
        };
        let exit_path = identity.mark_task_exited();
        (
            lease.expect("thread TID lease transferred twice"),
            exit_path,
        )
    }

    /// Releases the runtime link and TID role after scheduler-visible exit.
    ///
    /// The returned [`ExitPathLease`] keeps the identity's exit path pending
    /// until the caller completes it at the end of `do_exit`.
    pub(crate) fn retire_pid(&self) -> ExitPathLease {
        let (tid_lease, exit_path) = self.retire_pid_retaining_tid();
        exit_path.retain_tid(tid_lease)
    }

    /// Atomically transfers a fully retired leader identity to this runtime
    /// task at exec.
    ///
    /// The caller's previous identity is fully retired here, not left to a
    /// later `do_exit`: after the swap that exit runs under the leader
    /// identity and will never complete the previous one. The caller waits for
    /// the retired leader's exit-path lease before this transfer, matching
    /// Linux `de_thread`, which observes the old leader's exit state and runs
    /// its `release_task` path before adopting the leader PID.
    pub(crate) fn transfer_pid_identity(
        &self,
        task: &UserTaskRef,
        identity: Arc<PidIdentity>,
        tid_lease: PidRoleLease<Tid>,
    ) {
        let previous = {
            let mut pid = self.pid.lock();
            let _irq_guard = NoPreemptIrqSave::new();
            task.transfer_irq_pid_identity(&identity)
                .expect("exec leader identity differs from the cached process identity");
            core::mem::replace(
                &mut *pid,
                ThreadPidOwnership {
                    identity: identity.clone(),
                    tid_lease: Some(tid_lease),
                },
            )
        };
        // Dropping the TID role lease first releases the previous role before
        // the exit path completes, so a roleless identity can also detach its
        // namespace number in the same step.
        let previous_identity = {
            drop(previous.tid_lease);
            previous.identity
        };
        previous_identity.mark_task_exited().complete();
        identity.transfer_task(task, &self.proc_data.identity(), self.exit_flag());
    }

    /// Returns this Linux task's retained nice value.
    pub fn nice(&self) -> i32 {
        self.identity.nice.load(Ordering::Acquire)
    }

    /// Updates this Linux task's retained nice value.
    pub fn set_nice(&self, nice: i32) {
        self.identity.nice.store(nice, Ordering::Release);
    }

    /// Returns the generation-bearing scheduler identity, if bound.
    #[cfg(target_arch = "aarch64")]
    pub fn scheduler_id(&self) -> Option<ax_std::os::arceos::task::ThreadId> {
        self.identity.scheduler.get()
    }

    /// Binds the scheduler identity exactly once.
    pub(crate) fn bind_scheduler_id(
        &self,
        id: ax_std::os::arceos::task::ThreadId,
    ) -> crate::StarryResult<()> {
        self.identity.scheduler.bind(id)
    }

    pub(crate) fn validate_scheduler_id(
        &self,
        id: ax_std::os::arceos::task::ThreadId,
    ) -> crate::StarryResult<()> {
        self.identity.scheduler.validate_bound(id)
    }

    pub(super) fn scheduler_switch_in(
        &self,
        id: ax_std::os::arceos::task::ThreadId,
        realtime_policy: bool,
        cpu_pin: &CpuPin<'_>,
    ) {
        if self.validate_scheduler_id(id).is_err() {
            panic!("Starry thread was rebound to a different scheduler identity");
        }
        self.accounting
            .cpu_time
            .scheduler_switch_in(realtime_policy);
        // SAFETY: the scheduler switch baton pins this CPU and retains the
        // thread-owned ProcessData until the matching switch-out callback.
        unsafe { self.scope.activate_pinned(cpu_pin) };
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::perf_sched_in(self);
    }

    pub(super) fn scheduler_switch_out(
        &self,
        reason: ax_std::os::arceos::task::SwitchReason,
        observed_ns: u64,
        cpu_pin: &CpuPin<'_>,
    ) {
        #[cfg(target_arch = "aarch64")]
        crate::perf::task::perf_sched_out(self);
        // SAFETY: switch-in established exactly one activation for this task,
        // and the scheduler baton still pins the same CPU during switch-out.
        unsafe { self.scope.deactivate_pinned(cpu_pin) };
        self.accounting
            .cpu_time
            .scheduler_switch_out(reason, observed_ns);
        self.publish_cpu_time_for_active_interval_timer();
    }

    pub(crate) fn set_cpu_time_state(&self, state: TimerState) {
        self.accounting.cpu_time.set_state(state);
    }

    pub(crate) fn apply_cpu_time_policy(&self, realtime_policy: bool, observed_ns: u64) {
        self.accounting
            .cpu_time
            .apply_realtime_policy(realtime_policy, observed_ns);
        self.publish_cpu_time_for_active_interval_timer();
    }

    pub(crate) fn account_cpu_time_now(&self) {
        self.accounting.cpu_time.account_now();
        self.publish_cpu_time_for_active_interval_timer();
    }

    pub(crate) fn commit_cpu_time_now(&self) {
        self.accounting.cpu_time.account_now();
        self.proc_data.record_cpu_time_transition(|| {
            self.accounting.cpu_time.publish_committed_delta()
        });
    }

    pub(super) fn sample_scheduler_tick_cpu_time(&self, observed_ns: u64) {
        self.proc_data.record_cpu_time_transition(|| {
            self.accounting
                .cpu_time
                .sample_scheduler_tick_at(observed_ns)
        });
    }

    fn publish_cpu_time_for_active_interval_timer(&self) {
        if self.proc_data.has_active_cpu_interval_timers() {
            self.proc_data.record_cpu_time_transition(|| {
                self.accounting.cpu_time.publish_committed_delta()
            });
        }
    }

    pub(crate) fn cpu_time(&self) -> &CpuTimeAccounting {
        &self.accounting.cpu_time
    }

    pub(crate) fn rttime(&self) -> &PiMutex<RttimeWatchdog> {
        &self.accounting.rttime
    }

    /// Returns the clear-child-TID address.
    pub fn clear_child_tid(&self) -> usize {
        self.lifecycle.clear_child_tid.load(Ordering::Relaxed)
    }

    /// Updates the clear-child-TID address.
    pub fn set_clear_child_tid(&self, clear_child_tid: usize) {
        self.lifecycle
            .clear_child_tid
            .store(clear_child_tid, Ordering::Relaxed);
    }

    /// Returns the robust-list head address.
    pub fn robust_list_head(&self) -> usize {
        self.lifecycle.robust_list_head.load(Ordering::SeqCst)
    }

    /// Updates the robust-list head address.
    pub fn set_robust_list_head(&self, robust_list_head: usize) {
        self.lifecycle
            .robust_list_head
            .store(robust_list_head, Ordering::SeqCst);
    }

    /// Returns whether the thread exit transaction has completed.
    pub fn pending_exit(&self) -> bool {
        self.lifecycle.exit.load(Ordering::Acquire)
    }

    /// Claims this thread's exit transaction exactly once.
    pub fn begin_exit(&self) -> bool {
        self.lifecycle
            .exit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Publishes completion of the thread exit transaction.
    pub fn set_exit(&self) {
        self.lifecycle.exit.store(true, Ordering::Release);
    }

    pub(crate) fn exit_flag(&self) -> Arc<AtomicBool> {
        self.lifecycle.exit.clone()
    }

    pub(crate) fn exit_event(&self) -> Arc<PollSet> {
        self.lifecycle.exit_event.clone()
    }

    /// Consumes one pending thread-only exit request.
    pub fn take_exit_request(&self) -> bool {
        self.lifecycle.exit_request.consume()
    }

    /// Probes a pending thread-only exit request without consuming it.
    pub fn has_exit_request(&self) -> bool {
        self.lifecycle.exit_request.is_pending()
    }

    /// Requests a thread-only exit at the next signal safe point.
    pub fn set_exit_request(&self) {
        self.lifecycle.exit_request.publish();
    }

    pub(super) fn publish_deadline_overrun(&self) {
        self.lifecycle.deadline_overrun.publish();
    }

    pub(super) fn take_deadline_overrun(&self) -> bool {
        self.lifecycle.deadline_overrun.consume()
    }

    pub(crate) fn enter_user_memory_access(&self) -> UserMemoryAccessGuard<'_> {
        self.lifecycle.user_memory_access.enter()
    }

    pub(crate) fn has_active_user_memory_access(&self) -> bool {
        self.lifecycle.user_memory_access.is_active()
    }

    pub(super) fn interrupt(&self) {
        self.lifecycle.interrupted.publish();
    }

    pub(super) fn take_interrupt(&self) -> bool {
        self.lifecycle.interrupted.consume()
    }

    pub(super) fn interrupted(&self) -> bool {
        self.lifecycle.interrupted.is_pending()
    }

    pub(super) fn interrupt_snapshot(&self) -> InterruptSnapshot {
        self.lifecycle.interrupted.snapshot()
    }

    pub(super) fn acknowledge_interrupt(&self, snapshot: InterruptSnapshot) {
        let _advanced = self.lifecycle.interrupted.acknowledge(snapshot);
    }

    /// Returns the registered rseq area pointer.
    pub fn rseq_area(&self) -> usize {
        self.lifecycle.rseq_area.load(Ordering::SeqCst)
    }

    /// Returns the registered rseq signature.
    pub fn rseq_signature(&self) -> u32 {
        self.lifecycle.rseq_signature.load(Ordering::SeqCst)
    }

    /// Updates the registered rseq area and signature.
    pub fn set_rseq_state(&self, addr: usize, sig: u32) {
        self.lifecycle.rseq_area.store(addr, Ordering::SeqCst);
        self.lifecycle.rseq_signature.store(sig, Ordering::SeqCst);
    }

    /// Clears the registered rseq state.
    pub fn clear_rseq_state(&self) {
        self.lifecycle.rseq_area.store(0, Ordering::SeqCst);
        self.lifecycle.rseq_signature.store(0, Ordering::SeqCst);
    }

    /// Blocks the next signal check for this thread.
    pub fn block_next_signal_check(&self) {
        self.lifecycle.block_next_signal_check.block();
    }

    /// Consumes the one-shot signal-check block.
    pub fn unblock_next_signal_check(&self) -> bool {
        self.lifecycle.block_next_signal_check.unblock()
    }

    /// Returns this thread's signal manager.
    pub fn signal(&self) -> &Arc<ThreadSignalManager> {
        &self.signals.manager
    }

    /// Defers restoration of a temporary syscall signal mask until delivery.
    pub(crate) fn defer_signal_mask_restore(&self, mask: SignalSet) {
        let previous = self.signals.deferred_mask_restore.lock().replace(mask);
        assert!(
            previous.is_none(),
            "one thread cannot own nested deferred signal-mask restores"
        );
    }

    /// Takes the mask that the next delivered signal frame must restore.
    pub(crate) fn take_deferred_signal_mask_restore(&self) -> Option<SignalSet> {
        self.signals.deferred_mask_restore.lock().take()
    }

    pub(crate) fn wake_signalfd(&self) {
        // Pending signal state is published before pollers are woken.
        unsafe { self.signals.signalfd_waker.wake(axpoll::IoEvents::IN) };
    }

    pub(crate) fn signalfd_poll_source(&self) -> &PollSet {
        &self.signals.signalfd_waker
    }

    /// Returns the OOM score adjustment value.
    pub fn oom_score_adj(&self) -> i32 {
        self.security.oom_score_adj.load(Ordering::SeqCst)
    }

    /// Updates the OOM score adjustment value.
    pub fn set_oom_score_adj(&self, value: i32) {
        self.security.oom_score_adj.store(value, Ordering::SeqCst);
    }

    /// Returns the parent-death signal.
    pub fn pdeathsig(&self) -> u32 {
        self.security.pdeathsig.load(Ordering::Relaxed)
    }

    /// Updates the parent-death signal.
    pub fn set_pdeathsig(&self, sig: u32) {
        self.security.pdeathsig.store(sig, Ordering::Relaxed);
    }

    /// Returns whether no-new-privileges is active.
    pub fn no_new_privs(&self) -> bool {
        self.security.no_new_privs.load(Ordering::Relaxed)
    }

    /// Permanently enables no-new-privileges.
    pub fn set_no_new_privs(&self) {
        self.security.no_new_privs.store(true, Ordering::Relaxed);
    }

    /// Returns a snapshot of the seccomp state.
    pub fn seccomp_state(&self) -> Arc<SeccompState> {
        self.security.seccomp.snapshot()
    }

    /// Evaluates the immutable seccomp snapshot published for this thread.
    pub(crate) fn evaluate_seccomp(&self, uctx: &UserContext) -> SeccompDecision {
        self.security.seccomp.evaluate(uctx)
    }

    /// Replaces inherited seccomp state.
    pub fn set_seccomp_state(&self, state: Arc<SeccompState>) {
        self.security.seccomp.replace(state);
    }

    /// Enables strict seccomp mode.
    pub fn install_seccomp_strict(&self) -> crate::StarryResult<()> {
        self.security.seccomp.update(SeccompState::install_strict)
    }

    /// Appends one seccomp filter program.
    pub fn append_seccomp_filter(&self, insns: Vec<SockFilter>) -> crate::StarryResult<()> {
        self.security
            .seccomp
            .update(move |state| state.append_filter(insns))
    }

    /// Returns a credential snapshot.
    pub fn cred(&self) -> Arc<Cred> {
        self.security.cred.lock().clone()
    }

    fn set_cred_single(&self, new_cred: Arc<Cred>) {
        let previous = {
            let mut current = self.security.cred.lock();
            core::mem::replace(&mut *current, new_cred)
        };
        drop(previous);
    }

    /// Replaces credentials for this thread only.
    pub(crate) fn set_thread_cred(&self, new_cred: Cred) {
        self.set_cred_single(Arc::new(new_cred));
    }

    /// Replaces credentials for every thread in this process.
    pub fn set_cred(&self, new_cred: Cred) {
        let new_arc = Arc::new(new_cred);
        self.set_cred_single(new_arc.clone());

        let mut tids = self.proc_data.proc.threads();
        tids.sort_unstable();
        for tid in &tids {
            if let Ok(task) = ops::get_task_by_number(*tid) {
                task.as_thread().set_cred_single(new_arc.clone());
            }
        }
    }

    /// Updates every thread from its own credential snapshot.
    ///
    /// Process-wide set-ID transitions must preserve thread-local state such
    /// as `PR_SET_KEEPCAPS` while publishing the shared ID change.
    pub(crate) fn update_process_creds(&self, update: impl Fn(&Cred) -> Cred) {
        let old_cred = self.cred();
        self.set_cred_single(Arc::new(update(&old_cred)));

        let mut tids = self.proc_data.proc.threads();
        tids.sort_unstable();
        for tid in &tids {
            if let Ok(task) = ops::get_task_by_number(*tid) {
                let thread = task.as_thread();
                if core::ptr::eq(thread, self) {
                    continue;
                }
                let old_cred = thread.cred();
                thread.set_cred_single(Arc::new(update(&old_cred)));
            }
        }
    }

    /// Returns whether `uid_map` has been written.
    pub fn uid_map_written(&self) -> bool {
        self.security.uid_map_written.load(Ordering::Relaxed)
    }

    /// Updates the `uid_map` publication state.
    pub fn set_uid_map_written(&self, val: bool) {
        self.security.uid_map_written.store(val, Ordering::Relaxed);
    }

    /// Returns whether `gid_map` has been written.
    pub fn gid_map_written(&self) -> bool {
        self.security.gid_map_written.load(Ordering::Relaxed)
    }

    /// Updates the `gid_map` publication state.
    pub fn set_gid_map_written(&self, val: bool) {
        self.security.gid_map_written.store(val, Ordering::Relaxed);
    }

    /// Returns whether `setgroups` is denied.
    pub fn setgroups_deny(&self) -> bool {
        self.security.setgroups_deny.load(Ordering::Relaxed)
    }

    /// Updates the `setgroups` deny state.
    pub fn set_setgroups_deny(&self, val: bool) {
        self.security.setgroups_deny.store(val, Ordering::Relaxed);
    }

    pub(crate) fn claim_fault_dump(&self, signo: u8) -> bool {
        self.trace
            .fault_dump_signo
            .compare_exchange(signo, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub(crate) fn set_fault_dump(&self, signo: u8) {
        self.trace.fault_dump_signo.store(signo, Ordering::Release);
    }

    pub(crate) fn clear_fault_dump(&self) {
        self.trace.fault_dump_signo.store(0, Ordering::Release);
    }

    pub(super) fn push_kretprobe(&self, instance: kprobe::retprobe::RetprobeInstance) {
        let Some(mut stack) = self.trace.kretprobe_stack.try_lock() else {
            panic!("nested kretprobe tried to re-enter the current task stack");
        };
        if let Err(instance) = stack.try_push(instance) {
            core::mem::forget(instance);
            panic!("current task exceeded its fixed kretprobe nesting capacity");
        }
    }

    pub(super) fn pop_kretprobe(&self) -> kprobe::retprobe::RetprobeInstance {
        let Some(mut stack) = self.trace.kretprobe_stack.try_lock() else {
            panic!("nested kretprobe tried to re-enter the current task stack");
        };
        stack.pop().expect("kretprobe instance stack underflow")
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn perf_context(&self) -> &crate::perf::task_context::ThreadPerfContext {
        &self.trace.perf
    }
}

#[cfg(axtest)]
fn inactive_one_shot_flag_consumption_is_read_only_for_test() -> bool {
    let flag = OneShotFlag::new();
    if flag.consume() {
        return false;
    }
    flag.publish();
    flag.consume() && !flag.consume() && flag.consume_rmw_count() == 1
}

#[cfg(all(test, axtest))]
mod axtests {
    #[axtest::axtest]
    fn inactive_one_shot_flag_consumption_is_read_only() {
        assert!(super::inactive_one_shot_flag_consumption_is_read_only_for_test());
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::{NextSignalCheckBlock, ThreadSecurity};
    use crate::{sync::PiMutex, task::SeccompStateStore};

    #[test]
    fn seccomp_reads_use_an_immutable_snapshot_store() {
        fn assert_pi_mutex<T>(_: &PiMutex<T>) {}
        fn assert_seccomp_store(_: &SeccompStateStore) {}
        fn assert_security_lock_types(security: &ThreadSecurity) {
            assert_seccomp_store(&security.seccomp);
            assert_pi_mutex(&security.cred);
        }

        let _ = assert_security_lock_types as fn(&ThreadSecurity);
    }

    #[test]
    fn old_global_signal_check_block_leaks_between_threads() {
        static OLD_BLOCK_NEXT_SIGNAL_CHECK: AtomicBool = AtomicBool::new(false);

        fn block_next_signal() {
            OLD_BLOCK_NEXT_SIGNAL_CHECK.store(true, Ordering::SeqCst);
        }

        fn unblock_next_signal() -> bool {
            OLD_BLOCK_NEXT_SIGNAL_CHECK.swap(false, Ordering::SeqCst)
        }

        block_next_signal();
        assert!(unblock_next_signal());
        assert!(!unblock_next_signal());
    }

    #[test]
    fn per_thread_signal_check_block_is_isolated() {
        let thread_a = NextSignalCheckBlock::new();
        let thread_b = NextSignalCheckBlock::new();

        thread_a.block();

        assert!(!thread_b.unblock());
        assert!(thread_a.unblock());
        assert!(!thread_a.unblock());
    }
}

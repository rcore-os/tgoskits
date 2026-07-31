use super::*;

const IPI_CLAIMED: u64 = 1;
const CPU_LIFECYCLE_OFFLINE: usize = 1 << (usize::BITS - 1);
const CPU_LIFECYCLE_DRAINING: usize = 1 << (usize::BITS - 2);
const CPU_LIFECYCLE_MASK: usize = CPU_LIFECYCLE_OFFLINE | CPU_LIFECYCLE_DRAINING;
const CPU_PUBLICATION_COUNT_MASK: usize = !CPU_LIFECYCLE_MASK;
const CPU_PUBLICATION_OVERFLOW_INVARIANT: u32 = 0x4350_5542;
const CPU_PUBLICATION_RELEASE_INVARIANT: u32 = 0x4350_5544;
const IDLE_PULL_PHASE_MASK: u64 = 0b11;
const IDLE_PULL_IDLE: u64 = 0;
const IDLE_PULL_PENDING: u64 = 1;
const IDLE_PULL_CLAIMED: u64 = 2;
const IDLE_PULL_COMMITTED: u64 = 3;
const IDLE_PULL_PUBLISHER_SHIFT: u32 = 2;
const IDLE_PULL_PUBLISHER_BITS: u32 = 16;
const IDLE_PULL_PUBLISHER_ONE: u64 = 1 << IDLE_PULL_PUBLISHER_SHIFT;
const IDLE_PULL_PUBLISHER_MASK: u64 =
    ((1 << IDLE_PULL_PUBLISHER_BITS) - 1) << IDLE_PULL_PUBLISHER_SHIFT;
const IDLE_PULL_GENERATION_STEP: u64 = 1 << (IDLE_PULL_PUBLISHER_SHIFT + IDLE_PULL_PUBLISHER_BITS);
const IDLE_PULL_GENERATION_MASK: u64 = !(IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK);
const IDLE_PULL_PUBLISHER_OVERFLOW_INVARIANT: u32 = 0x4944_4c50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchedulerIpiClaim(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdlePullReservation {
    Started(u64),
    AlreadyPending,
    Busy,
}

pub(crate) struct IdlePullClaim<'remote> {
    remote: &'remote CpuRemote,
    state: u64,
}

impl IdlePullClaim<'_> {
    /// Linearizes the pull before the target admits newer runnable work.
    pub(crate) fn commit(&mut self) -> bool {
        let committed = (self.state & !IDLE_PULL_PHASE_MASK) | IDLE_PULL_COMMITTED;
        if self
            .remote
            .idle_pull_state
            .compare_exchange(self.state, committed, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.state = committed;
        true
    }
}

impl Drop for IdlePullClaim<'_> {
    fn drop(&mut self) {
        let generation = self.state & IDLE_PULL_GENERATION_MASK;
        let phase = self.state & IDLE_PULL_PHASE_MASK;
        let mut current = self.remote.idle_pull_state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_GENERATION_MASK != generation
                || current & IDLE_PULL_PHASE_MASK != phase
            {
                return;
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.remote.idle_pull_state.compare_exchange_weak(
                current,
                idle,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

pub(crate) struct IdlePullWorkPublication<'remote> {
    remote: &'remote CpuRemote,
}

impl Drop for IdlePullWorkPublication<'_> {
    fn drop(&mut self) {
        let previous = self
            .remote
            .idle_pull_state
            .fetch_sub(IDLE_PULL_PUBLISHER_ONE, Ordering::Release);
        debug_assert_ne!(
            previous & IDLE_PULL_PUBLISHER_MASK,
            0,
            "idle-pull work publisher count underflowed"
        );
    }
}

/// Placement and remote-publication state of one logical CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuLifecycleState {
    /// The CPU accepts placement and remote scheduler publications.
    Online,
    /// New placement is closed while the owner proves that all work is gone.
    Draining,
    /// The CPU owns no schedulable work and is absent from the root domain.
    Offline,
}

/// Stable cross-CPU publication endpoint for one scheduler owner.
///
/// This object contains only atomic state and intrusive MPSC inboxes. It is
/// allocated separately from [`CpuLocal`], so remote producers never create a
/// shared reference to the owner-only runqueue object while its CPU holds a
/// unique mutable borrow.
#[derive(Debug)]
pub struct CpuRemote {
    owner: CpuId,
    owner_claimed: AtomicBool,
    /// Top bits encode [`CpuLifecycleState`]; low bits count active publishers.
    lifecycle: AtomicUsize,
    scheduler_ready: AtomicBool,
    need_resched: AtomicBool,
    deadline_work_pending: AtomicBool,
    preempt_requested: AtomicBool,
    park_preempt_deferred: AtomicBool,
    scheduler_ipi_pending: AtomicU64,
    idle_polling: AtomicBool,
    current_thread: AtomicU64,
    idle_thread: AtomicU64,
    busy_runtime_ns: AtomicU64,
    load_summary_sequence: AtomicU64,
    load_summary_runnable: AtomicUsize,
    load_summary_flags: AtomicU8,
    load_summary_current_primary: AtomicU64,
    load_summary_current_sequence: AtomicU64,
    load_summary_pushable_primary: AtomicU64,
    load_summary_pushable_sequence: AtomicU64,
    idle_pull_state: AtomicU64,
    pub(super) fair_balance_deadline_ns: AtomicU64,
    pub(super) scheduler_deadline_ns: AtomicU64,
    remote_wake_inbox: SchedulerInbox,
    migration_inbox: SchedulerInbox,
    reclaim_inbox: SchedulerInbox,
    balance_request_node: InboxNode,
}

impl CpuRemote {
    pub(crate) fn create(owner: CpuId) -> Arc<Self> {
        Arc::new(Self {
            owner,
            owner_claimed: AtomicBool::new(false),
            lifecycle: AtomicUsize::new(CPU_LIFECYCLE_OFFLINE),
            scheduler_ready: AtomicBool::new(false),
            need_resched: AtomicBool::new(false),
            deadline_work_pending: AtomicBool::new(false),
            preempt_requested: AtomicBool::new(false),
            park_preempt_deferred: AtomicBool::new(false),
            scheduler_ipi_pending: AtomicU64::new(0),
            idle_polling: AtomicBool::new(false),
            current_thread: AtomicU64::new(0),
            idle_thread: AtomicU64::new(0),
            busy_runtime_ns: AtomicU64::new(0),
            load_summary_sequence: AtomicU64::new(0),
            load_summary_runnable: AtomicUsize::new(0),
            load_summary_flags: AtomicU8::new(0),
            load_summary_current_primary: AtomicU64::new(0),
            load_summary_current_sequence: AtomicU64::new(0),
            load_summary_pushable_primary: AtomicU64::new(0),
            load_summary_pushable_sequence: AtomicU64::new(0),
            idle_pull_state: AtomicU64::new(IDLE_PULL_IDLE),
            // An offline CPU has no monotonic time origin yet. Publishing a
            // duration here as an absolute deadline makes every CPU brought
            // online after that duration immediately overdue.
            fair_balance_deadline_ns: AtomicU64::new(u64::MAX),
            scheduler_deadline_ns: AtomicU64::new(0),
            remote_wake_inbox: SchedulerInbox::new(InboxKind::RemoteWake),
            migration_inbox: SchedulerInbox::new(InboxKind::Migration),
            reclaim_inbox: SchedulerInbox::new(InboxKind::Reclaim),
            balance_request_node: InboxNode::new(InboxKind::Migration),
        })
    }

    /// Returns the CPU that owns the corresponding runqueue.
    pub const fn owner(&self) -> CpuId {
        self.owner
    }

    /// Claims exclusive access to the corresponding owner-only scheduler object.
    ///
    /// # Safety
    ///
    /// `cpu` must identify the pinned, live [`CpuLocal`] associated with this
    /// endpoint. After runtime publication, every access that can overlap this
    /// claim must use the same endpoint rather than retaining an ungated borrow.
    pub unsafe fn claim_local(
        &self,
        cpu: *mut CpuLocal,
    ) -> Result<CpuLocalOwnerBorrow<'_>, TaskError> {
        let cpu = NonNull::new(cpu).ok_or(TaskError::InvalidRuntimeHandle)?;
        self.owner_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| TaskError::CpuOwnerBorrowed)?;

        // SAFETY: the caller guarantees that this is the live pinned CpuLocal
        // paired with this endpoint. The successful gate claim excludes every
        // other runtime-derived reference while the identity is checked.
        let actual = unsafe { cpu.as_ref() }.owner();
        if actual != self.owner {
            self.owner_claimed.store(false, Ordering::Release);
            return Err(TaskError::CpuOwnerMismatch {
                expected: self.owner.as_u32(),
                actual: actual.as_u32(),
            });
        }
        Ok(CpuLocalOwnerBorrow {
            remote: self,
            cpu,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Returns the generation-bearing current-thread snapshot.
    pub fn current_thread(&self) -> Option<ThreadId> {
        decode_thread_id(self.current_thread.load(Ordering::Acquire))
    }

    /// Returns the configured idle-thread snapshot.
    pub fn idle_thread(&self) -> Option<ThreadId> {
        decode_thread_id(self.idle_thread.load(Ordering::Acquire))
    }

    pub(crate) fn publish_current_thread(&self, current: Option<ThreadId>) {
        self.current_thread
            .store(current.map_or(0, ThreadId::as_u64), Ordering::Release);
    }

    pub(super) fn publish_idle_thread(&self, idle: ThreadId) {
        self.idle_thread.store(idle.as_u64(), Ordering::Release);
    }

    /// Returns cumulative time this CPU has executed non-idle scheduler threads.
    pub fn busy_runtime_ns(&self) -> u64 {
        self.busy_runtime_ns.load(Ordering::Relaxed)
    }

    pub(super) fn charge_busy_runtime(&self, runtime_ns: u64) {
        self.busy_runtime_ns
            .fetch_add(runtime_ns, Ordering::Relaxed);
    }

    /// Returns the CPU's placement and publication lifecycle.
    pub fn lifecycle_state(&self) -> CpuLifecycleState {
        match self.lifecycle.load(Ordering::Acquire) & CPU_LIFECYCLE_MASK {
            0 => CpuLifecycleState::Online,
            CPU_LIFECYCLE_DRAINING => CpuLifecycleState::Draining,
            _ => CpuLifecycleState::Offline,
        }
    }

    /// Returns whether owner initialization and online publication completed.
    pub fn is_online(&self) -> bool {
        self.lifecycle_state() == CpuLifecycleState::Online
    }

    pub(crate) fn mark_online(&self) -> bool {
        self.lifecycle
            .compare_exchange(
                CPU_LIFECYCLE_OFFLINE,
                0,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn try_begin_draining(&self) -> bool {
        // Matching the exact zero-valued Online state also proves that no
        // producer currently spans queue publication and its doorbell.
        let draining = self
            .lifecycle
            .compare_exchange(
                0,
                CPU_LIFECYCLE_DRAINING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if draining {
            self.cancel_idle_pull_if_uncommitted();
        }
        draining
    }

    pub(crate) fn cancel_draining(&self) {
        if self
            .lifecycle
            .compare_exchange(
                CPU_LIFECYCLE_DRAINING,
                0,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            task_runtime::fatal_invariant(
                CPU_PUBLICATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn finish_offline(&self) {
        self.need_resched.store(false, Ordering::Relaxed);
        self.deadline_work_pending.store(false, Ordering::Relaxed);
        self.preempt_requested.store(false, Ordering::Relaxed);
        self.park_preempt_deferred.store(false, Ordering::Relaxed);
        self.scheduler_ipi_pending.store(0, Ordering::Relaxed);
        self.idle_polling.store(false, Ordering::Relaxed);
        self.fair_balance_deadline_ns
            .store(u64::MAX, Ordering::Relaxed);
        self.scheduler_deadline_ns.store(0, Ordering::Relaxed);
        if self
            .lifecycle
            .compare_exchange(
                CPU_LIFECYCLE_DRAINING,
                CPU_LIFECYCLE_OFFLINE,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            task_runtime::fatal_invariant(
                CPU_PUBLICATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn begin_publication(&self) -> Option<CpuRemotePublication<'_>> {
        let mut current = self.lifecycle.load(Ordering::Acquire);
        loop {
            if current & CPU_LIFECYCLE_MASK != 0 {
                return None;
            }
            let count = current & CPU_PUBLICATION_COUNT_MASK;
            if count == CPU_PUBLICATION_COUNT_MASK {
                task_runtime::fatal_invariant(
                    CPU_PUBLICATION_OVERFLOW_INVARIANT,
                    self.owner.as_u32() as usize,
                );
            }
            match self.lifecycle.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(CpuRemotePublication { remote: self }),
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn is_quiescent_for_offline(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == CPU_LIFECYCLE_DRAINING
            && !self.deadline_work_pending()
            && !self.has_remote_work()
            && self.scheduler_ipi_pending.load(Ordering::Acquire) & IPI_CLAIMED == 0
            && !self.is_idle_polling()
            && self.idle_pull_state.load(Ordering::Acquire)
                & (IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK)
                == IDLE_PULL_IDLE
    }

    pub(crate) fn mark_scheduler_ready(&self) {
        self.scheduler_ready.store(true, Ordering::Release);
    }

    pub(crate) fn is_scheduler_ready(&self) -> bool {
        self.scheduler_ready.load(Ordering::Acquire)
    }

    /// Publishes a sticky owner-CPU reschedule request.
    pub(crate) fn request_reschedule(&self) {
        let Some(_publication) = self.begin_publication() else {
            return;
        };
        self.request_reschedule_owned();
    }

    fn request_reschedule_owned(&self) {
        self.preempt_requested.store(true, Ordering::Release);
        self.need_resched.store(true, Ordering::Release);
    }

    pub(crate) fn request_scheduler_work(&self) {
        let Some(_publication) = self.begin_publication() else {
            return;
        };
        self.request_scheduler_work_owned();
    }

    fn request_scheduler_work_owned(&self) {
        self.need_resched.store(true, Ordering::Release);
    }

    pub(super) fn publish_deadline_work(&self) {
        self.deadline_work_pending.store(true, Ordering::Release);
        self.request_scheduler_work_owned();
    }

    pub(crate) fn deadline_work_pending(&self) -> bool {
        self.deadline_work_pending.load(Ordering::Acquire)
    }

    pub(super) fn begin_deadline_work(&self) -> bool {
        self.deadline_work_pending.swap(false, Ordering::AcqRel)
    }

    pub(super) fn finish_deadline_work(&self, pending: bool) {
        // Only the owner CPU publishes deadline work, and both timer IRQ and
        // scheduler safe-point paths hold local IRQ exclusion while mutating
        // CpuLocal. The completed pass therefore owns the full publication
        // interval and may replace the sticky bit with its actual remainder.
        self.deadline_work_pending.store(pending, Ordering::Release);
        if pending {
            self.request_scheduler_work_owned();
        }
    }

    pub(crate) fn kick_scheduler_work(&self) -> bool {
        let Some(_publication) = self.begin_publication() else {
            return false;
        };
        let _irq = IrqScope::enter();
        self.kick_scheduler_work_owned()
    }

    fn kick_scheduler_work_owned(&self) -> bool {
        self.request_scheduler_work_owned();
        if self.current_cpu_will_service_local_work() {
            return true;
        }
        let Some(claim) = self.claim_scheduler_ipi() else {
            return false;
        };
        self.send_claimed_scheduler_ipi(claim);
        true
    }

    fn current_cpu_will_service_local_work(&self) -> bool {
        // Every caller retains an IrqScope from before this observation through
        // publication completion, so the runtime CPU identity cannot migrate.
        let current = unsafe { task_runtime::current_cpu_id() };
        if current.as_u32() != self.owner.as_u32() {
            return false;
        }
        // Hard IRQ return consumes the sticky request through its outer
        // preemption guard. Ordinary task publication instead converts the
        // final IRQ guard directly into the scheduler baton. In both cases a
        // self-IPI would add an unnecessary interrupt round trip.
        task_runtime::in_hard_irq() || task_runtime::local_scheduler_work_is_self_serviced()
    }

    fn claim_scheduler_ipi(&self) -> Option<SchedulerIpiClaim> {
        let mut current = self.scheduler_ipi_pending.load(Ordering::Acquire);
        loop {
            if current & IPI_CLAIMED != 0 {
                return None;
            }
            let next = current.wrapping_add(2) | IPI_CLAIMED;
            match self.scheduler_ipi_pending.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(SchedulerIpiClaim(next)),
                Err(actual) => current = actual,
            }
        }
    }

    fn finish_scheduler_ipi_send(&self, _claim: SchedulerIpiClaim, status: RuntimeStatus) {
        match status {
            RuntimeStatus::Success => {}
            // Like Linux's CSD `-EBUSY`, this status means an older physical
            // delivery is still in flight and covers the newly published work.
            // The target handler owns and clears the claim; clearing it here
            // would replace a guaranteed interrupt with wake-less retry state.
            RuntimeStatus::Busy => {}
            status => task_runtime::fatal_invariant(
                0x4950_4900 | status as u32,
                self.owner.as_u32() as usize,
            ),
        }
    }

    /// Completes one already-claimed doorbell transaction and always feeds the
    /// typed transport result back into the coalescing/retry state machine.
    fn send_claimed_scheduler_ipi(&self, claim: SchedulerIpiClaim) {
        let status = task_runtime::send_scheduler_ipi(RuntimeCpuId::new(self.owner.as_u32()));
        self.finish_scheduler_ipi_send(claim, status);
    }

    /// Tests the sticky reschedule request without consuming it.
    pub fn needs_reschedule(&self) -> bool {
        self.need_resched.load(Ordering::Acquire)
    }

    pub(crate) fn scheduler_enter(&self) -> bool {
        self.need_resched.swap(false, Ordering::AcqRel);
        let preempt_requested = self.preempt_requested.swap(false, Ordering::AcqRel);
        if self.deadline_work_pending() || self.has_remote_work() {
            self.request_scheduler_work();
        }
        preempt_requested
    }

    #[cfg(test)]
    pub(crate) fn take_preempt_requested(&self) -> bool {
        self.preempt_requested.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn defer_park_preemption(&self, requested: bool) {
        if requested {
            self.park_preempt_deferred.store(true, Ordering::Release);
        }
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        let deferred = self.park_preempt_deferred.swap(false, Ordering::AcqRel);
        if resume_running && deferred {
            self.request_reschedule_owned();
        }
    }

    pub(crate) fn publish_remote_wake(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let Some(_remote_publication) = self.begin_publication() else {
            return PublishResult::WrongKind;
        };
        let _irq = IrqScope::enter();
        let _idle_pull_work = self.begin_idle_pull_work();
        let (result, _head_became_non_empty) = self
            .remote_wake_inbox
            .publish_with_head_transition(node, message);
        if matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ) {
            self.kick_scheduler_work_owned();
        }
        result
    }

    pub(crate) fn publish_policy_update(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let Some(remote_publication) = self.begin_publication() else {
            return PublishResult::WrongKind;
        };
        remote_publication.publish_policy_update(node, message)
    }

    fn publish_policy_update_owned(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let _irq = IrqScope::enter();
        let _idle_pull_work = self.begin_idle_pull_work();
        let (result, _head_became_non_empty) = self
            .migration_inbox
            .publish_with_head_transition(node, message);
        if matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ) {
            self.kick_scheduler_work_owned();
        }
        result
    }

    pub(crate) fn publish_migration(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        self.publish_policy_update(node, message)
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        let node = &self.balance_request_node as *const InboxNode;
        // SAFETY: TaskSystem owns this Arc-backed endpoint until shutdown. The
        // embedded node is never moved and coalesces publications for one CPU.
        unsafe { Pin::new_unchecked(&*node) }
    }

    pub(crate) fn begin_idle_pull(&self) -> IdlePullReservation {
        let mut current = self.idle_pull_state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_PUBLISHER_MASK != 0 {
                return IdlePullReservation::Busy;
            }
            if current & IDLE_PULL_PHASE_MASK != IDLE_PULL_IDLE {
                return IdlePullReservation::AlreadyPending;
            }
            let generation = (current & IDLE_PULL_GENERATION_MASK)
                .wrapping_add(IDLE_PULL_GENERATION_STEP)
                & IDLE_PULL_GENERATION_MASK;
            let pending = generation | IDLE_PULL_PENDING;
            match self.idle_pull_state.compare_exchange_weak(
                current,
                pending,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return IdlePullReservation::Started(pending),
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn cancel_idle_pull(&self, reservation: u64) {
        let generation = reservation & IDLE_PULL_GENERATION_MASK;
        let mut current = self.idle_pull_state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_GENERATION_MASK != generation
                || !matches!(
                    current & IDLE_PULL_PHASE_MASK,
                    IDLE_PULL_PENDING | IDLE_PULL_CLAIMED
                )
            {
                return;
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.idle_pull_state.compare_exchange_weak(
                current,
                idle,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn cancel_idle_pull_if_uncommitted(&self) {
        let mut current = self.idle_pull_state.load(Ordering::Acquire);
        loop {
            match current & IDLE_PULL_PHASE_MASK {
                IDLE_PULL_IDLE | IDLE_PULL_COMMITTED => return,
                IDLE_PULL_PENDING | IDLE_PULL_CLAIMED => {}
                _ => unreachable!(),
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.idle_pull_state.compare_exchange_weak(
                current,
                idle,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn begin_idle_pull_work(&self) -> IdlePullWorkPublication<'_> {
        let mut current = self.idle_pull_state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_PUBLISHER_MASK == IDLE_PULL_PUBLISHER_MASK {
                task_runtime::fatal_invariant(
                    IDLE_PULL_PUBLISHER_OVERFLOW_INVARIANT,
                    self.owner.as_u32() as usize,
                );
            }
            let phase = match current & IDLE_PULL_PHASE_MASK {
                IDLE_PULL_PENDING | IDLE_PULL_CLAIMED => IDLE_PULL_IDLE,
                phase => phase,
            };
            let next = ((current + IDLE_PULL_PUBLISHER_ONE) & !IDLE_PULL_PHASE_MASK) | phase;
            match self.idle_pull_state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return IdlePullWorkPublication { remote: self },
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn claim_idle_pull(&self, reservation: u64) -> Option<IdlePullClaim<'_>> {
        if reservation & (IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK) != IDLE_PULL_PENDING {
            return None;
        }
        let claimed = (reservation & !IDLE_PULL_PHASE_MASK) | IDLE_PULL_CLAIMED;
        self.idle_pull_state
            .compare_exchange(reservation, claimed, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| IdlePullClaim {
                remote: self,
                state: claimed,
            })
    }

    pub(crate) fn publish_load_summary(
        &self,
        current_key: Option<SchedulingKey>,
        pushable_key: Option<SchedulingKey>,
        runnable_count: usize,
        overloaded: bool,
    ) {
        let write_sequence = self.load_summary_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(write_sequence & 1, 0, "load summary has one owner writer");
        self.load_summary_runnable
            .store(runnable_count, Ordering::Relaxed);
        let mut flags = 0;
        if let Some(key) = current_key {
            flags |= SUMMARY_CURRENT_PRESENT;
            flags |= (key.class_rank() & SUMMARY_CLASS_MASK) << SUMMARY_CURRENT_CLASS_SHIFT;
            self.load_summary_current_primary
                .store(key.primary(), Ordering::Relaxed);
            self.load_summary_current_sequence
                .store(key.sequence(), Ordering::Relaxed);
        }
        if let Some(key) = pushable_key {
            flags |= SUMMARY_PUSHABLE_PRESENT;
            flags |= (key.class_rank() & SUMMARY_CLASS_MASK) << SUMMARY_PUSHABLE_CLASS_SHIFT;
            self.load_summary_pushable_primary
                .store(key.primary(), Ordering::Relaxed);
            self.load_summary_pushable_sequence
                .store(key.sequence(), Ordering::Relaxed);
        }
        if overloaded {
            flags |= SUMMARY_OVERLOADED;
        }
        self.load_summary_flags.store(flags, Ordering::Relaxed);
        self.load_summary_sequence.fetch_add(1, Ordering::Release);
    }

    /// Attempts to return a coherent remotely observable scheduling snapshot.
    ///
    /// The owner publishes under a local IRQ guard, but a remote CPU must not
    /// wait indefinitely if that owner is stopped or fails while its sequence
    /// is odd. Callers treat `None` as an unavailable placement candidate and
    /// retry from a later scheduler safe point.
    pub fn try_load_summary(&self) -> Option<CpuLoadSummary> {
        for _ in 0..LOAD_SUMMARY_READ_RETRIES {
            let epoch = self.load_summary_sequence.load(Ordering::Acquire);
            if epoch & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let runnable_count = self.load_summary_runnable.load(Ordering::Relaxed);
            let flags = self.load_summary_flags.load(Ordering::Relaxed);
            let current_primary = self.load_summary_current_primary.load(Ordering::Relaxed);
            let current_sequence = self.load_summary_current_sequence.load(Ordering::Relaxed);
            let pushable_primary = self.load_summary_pushable_primary.load(Ordering::Relaxed);
            let pushable_sequence = self.load_summary_pushable_sequence.load(Ordering::Relaxed);
            if self.load_summary_sequence.load(Ordering::Acquire) != epoch {
                continue;
            }
            let current_rank = (flags >> SUMMARY_CURRENT_CLASS_SHIFT) & SUMMARY_CLASS_MASK;
            let pushable_rank = (flags >> SUMMARY_PUSHABLE_CLASS_SHIFT) & SUMMARY_CLASS_MASK;
            return Some(CpuLoadSummary {
                epoch,
                runnable_count,
                current_key: (flags & SUMMARY_CURRENT_PRESENT != 0)
                    .then(|| SchedulingKey::new(current_rank, current_primary, current_sequence)),
                pushable_key: (flags & SUMMARY_PUSHABLE_PRESENT != 0).then(|| {
                    SchedulingKey::new(pushable_rank, pushable_primary, pushable_sequence)
                }),
                pushable_class: (flags & SUMMARY_PUSHABLE_PRESENT != 0)
                    .then(|| SchedulingClass::from_rank(pushable_rank)),
                overloaded: flags & SUMMARY_OVERLOADED != 0,
            });
        }
        None
    }

    /// Attempts to return the remotely observable queued runnable count.
    pub fn try_runnable_summary(&self) -> Option<usize> {
        self.try_load_summary().map(CpuLoadSummary::runnable_count)
    }

    pub(crate) fn fair_balance_due(&self, now_ns: u64) -> bool {
        now_ns >= self.fair_balance_deadline_ns.load(Ordering::Acquire)
    }

    pub(crate) fn defer_fair_balance(&self, now_ns: u64, interval_ns: u64) {
        self.fair_balance_deadline_ns
            .store(now_ns.saturating_add(interval_ns.max(1)), Ordering::Release);
    }

    pub(crate) fn remote_wake_inbox(&self) -> &SchedulerInbox {
        &self.remote_wake_inbox
    }

    pub(crate) fn migration_inbox(&self) -> &SchedulerInbox {
        &self.migration_inbox
    }

    pub(crate) fn reclaim_inbox(&self) -> &SchedulerInbox {
        &self.reclaim_inbox
    }

    pub(crate) fn has_remote_work(&self) -> bool {
        self.remote_wake_inbox.has_pending()
            || self.migration_inbox.has_pending()
            || self.reclaim_inbox.has_pending()
    }

    /// Acknowledges one coalesced scheduler IPI epoch and rechecks publication.
    pub(crate) fn acknowledge_scheduler_ipi(&self) {
        let mut current = self.scheduler_ipi_pending.load(Ordering::Acquire);
        while current & IPI_CLAIMED != 0 {
            match self.scheduler_ipi_pending.compare_exchange_weak(
                current,
                current & !IPI_CLAIMED,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        if self.deadline_work_pending() || self.has_remote_work() {
            self.request_scheduler_work();
        }
    }

    pub(crate) fn prepare_idle_wait(&self) -> bool {
        self.idle_polling.store(true, Ordering::Release);
        core::sync::atomic::fence(Ordering::SeqCst);
        let may_wait = !self.needs_reschedule()
            && !self.deadline_work_pending()
            && !self.has_remote_work()
            && self.try_runnable_summary() == Some(0);
        if !may_wait {
            self.idle_polling.store(false, Ordering::Release);
        }
        may_wait
    }

    pub(crate) fn finish_idle_wait(&self) {
        self.idle_polling.store(false, Ordering::Release);
    }

    pub(crate) fn is_idle_polling(&self) -> bool {
        self.idle_polling.load(Ordering::Acquire)
    }
}

pub(crate) struct CpuRemotePublication<'remote> {
    remote: &'remote CpuRemote,
}

impl CpuRemotePublication<'_> {
    pub(crate) fn publish_policy_update(
        self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        self.remote.publish_policy_update_owned(node, message)
    }
}

impl Drop for CpuRemotePublication<'_> {
    fn drop(&mut self) {
        let mut current = self.remote.lifecycle.load(Ordering::Acquire);
        loop {
            if current & CPU_LIFECYCLE_MASK != 0 || current & CPU_PUBLICATION_COUNT_MASK == 0 {
                task_runtime::fatal_invariant(
                    CPU_PUBLICATION_RELEASE_INVARIANT,
                    self.remote.owner.as_u32() as usize,
                );
            }
            match self.remote.lifecycle.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

/// Dynamically checked owner borrow of one pinned [`CpuLocal`].
///
/// The borrow gate resides in the separately allocated [`CpuRemote`] endpoint,
/// so a reentrant claim can fail without touching memory covered by the active
/// mutable `CpuLocal` reference.
pub struct CpuLocalOwnerBorrow<'remote> {
    remote: &'remote CpuRemote,
    cpu: NonNull<CpuLocal>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CpuLocalOwnerBorrow<'_> {
    /// Borrows the pinned owner state mutably for one audited call scope.
    pub fn as_pin_mut(&mut self) -> Pin<&mut CpuLocal> {
        // SAFETY: construction claimed the unique runtime owner gate, the
        // pointer remains pinned, and the returned lifetime is bounded by the
        // mutable borrow of this gate-owning wrapper.
        unsafe { Pin::new_unchecked(self.cpu.as_mut()) }
    }
}

impl Deref for CpuLocalOwnerBorrow<'_> {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the wrapper owns the endpoint's exclusive claim and its
        // lifetime is bounded by that claim.
        unsafe { self.cpu.as_ref() }
    }
}

impl Drop for CpuLocalOwnerBorrow<'_> {
    fn drop(&mut self) {
        self.remote.owner_claimed.store(false, Ordering::Release);
    }
}

fn decode_thread_id(raw: u64) -> Option<ThreadId> {
    (raw != 0).then(|| ThreadId::from_parts(raw as u32, (raw >> 32) as u32))
}

include!("remote/tests.rs");

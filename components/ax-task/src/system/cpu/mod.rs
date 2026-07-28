//! Pinned owner-CPU scheduler state.

mod dispatch;
mod snapshot;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    marker::{PhantomData, PhantomPinned},
    ops::Deref,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

pub(crate) use dispatch::{CurrentDispatch, CurrentDispatchState, DispatchCharge, SwitchHandoff};
pub use snapshot::CpuSnapshot;

use crate::{
    CpuId, DeadlineAdmission, FairMode, RtBandwidth, RunQueue, SchedulePolicy, SchedulingEntity,
    SchedulingKey, TaskError, TaskSystemConfig, ThreadHandle, ThreadId, ThreadState,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    lock::IrqScope,
    runtime::{MonotonicDeadline, RuntimeCpuId, RuntimeStatus, TaskDeadlineUpdate, task_runtime},
    thread::ThreadCore,
    timer::{
        ExpiredTaskDeadline, TaskDeadlineExpireBatch, TaskDeadlineExpireRequest, TaskDeadlineQueue,
    },
};

/// Scheduler class carried by a remotely observed CPU load summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SchedulingClass {
    /// Absolute-deadline EDF work.
    Deadline = 0,
    /// Fixed-priority FIFO or round-robin work.
    Realtime = 1,
    /// Normal or batch EEVDF work.
    Fair     = 2,
    /// Lowest-priority fair idle work.
    Idle     = 3,
}

impl SchedulingClass {
    const fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::Deadline,
            1 => Self::Realtime,
            2 => Self::Fair,
            _ => Self::Idle,
        }
    }
}

/// Coherent, allocation-free snapshot used by remote placement and balancing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuLoadSummary {
    epoch: u64,
    runnable_count: usize,
    current_key: Option<SchedulingKey>,
    pushable_key: Option<SchedulingKey>,
    pushable_class: Option<SchedulingClass>,
    overloaded: bool,
}

/// Per-runqueue GRUB utilization snapshot in billionths of one CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineBandwidthSnapshot {
    this_bw_scaled: u64,
    running_bw_scaled: u64,
    max_bw_scaled: u64,
}

impl DeadlineBandwidthSnapshot {
    /// Returns all Deadline utilization assigned to this runqueue.
    pub const fn this_bw_scaled(self) -> u64 {
        self.this_bw_scaled
    }

    /// Returns ActiveContending plus ActiveNonContending utilization.
    pub const fn running_bw_scaled(self) -> u64 {
        self.running_bw_scaled
    }

    /// Returns utilization currently eligible for GRUB reclaim.
    pub const fn inactive_bw_scaled(self) -> u64 {
        self.this_bw_scaled.saturating_sub(self.running_bw_scaled)
    }

    /// Returns the per-CPU reclaim capacity.
    pub const fn max_bw_scaled(self) -> u64 {
        self.max_bw_scaled
    }
}

impl CpuLoadSummary {
    /// Returns the publication epoch read with this coherent snapshot.
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Returns queued non-idle work owned by this CPU.
    pub const fn runnable_count(self) -> usize {
        self.runnable_count
    }

    /// Returns the effective urgency of the current dispatch, including PI.
    pub const fn current_key(self) -> Option<SchedulingKey> {
        self.current_key
    }

    /// Returns the most urgent queued candidate that can leave this CPU.
    pub const fn pushable_key(self) -> Option<SchedulingKey> {
        self.pushable_key
    }

    /// Returns the scheduler class of the top pushable candidate.
    pub const fn pushable_class(self) -> Option<SchedulingClass> {
        self.pushable_class
    }

    /// Reports whether this CPU owns more runnable work than it can execute.
    pub const fn is_overloaded(self) -> bool {
        self.overloaded
    }
}

const SUMMARY_CURRENT_PRESENT: u8 = 1 << 0;
const SUMMARY_PUSHABLE_PRESENT: u8 = 1 << 1;
const SUMMARY_OVERLOADED: u8 = 1 << 2;
const SUMMARY_CURRENT_CLASS_SHIFT: u32 = 3;
const SUMMARY_PUSHABLE_CLASS_SHIFT: u32 = 5;
const SUMMARY_CLASS_MASK: u8 = 0b11;
const LOAD_SUMMARY_READ_RETRIES: usize = 8;
const IPI_CLAIMED: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchedulerIpiClaim(u64);

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
    online: AtomicBool,
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
    fair_balance_deadline_ns: AtomicU64,
    scheduler_deadline_ns: AtomicU64,
    deferred_scheduler_deadline_ns: AtomicU64,
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
            online: AtomicBool::new(false),
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
            // An offline CPU has no monotonic time origin yet. Publishing a
            // duration here as an absolute deadline makes every CPU brought
            // online after that duration immediately overdue.
            fair_balance_deadline_ns: AtomicU64::new(u64::MAX),
            scheduler_deadline_ns: AtomicU64::new(0),
            deferred_scheduler_deadline_ns: AtomicU64::new(0),
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

    fn publish_idle_thread(&self, idle: ThreadId) {
        self.idle_thread.store(idle.as_u64(), Ordering::Release);
    }

    /// Returns cumulative time this CPU has executed non-idle scheduler threads.
    pub fn busy_runtime_ns(&self) -> u64 {
        self.busy_runtime_ns.load(Ordering::Relaxed)
    }

    fn charge_busy_runtime(&self, runtime_ns: u64) {
        self.busy_runtime_ns
            .fetch_add(runtime_ns, Ordering::Relaxed);
    }

    /// Returns whether owner initialization and online publication completed.
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }

    pub(crate) fn mark_online(&self) {
        self.online.store(true, Ordering::Release);
    }

    pub(crate) fn mark_scheduler_ready(&self) {
        self.scheduler_ready.store(true, Ordering::Release);
    }

    pub(crate) fn is_scheduler_ready(&self) -> bool {
        self.scheduler_ready.load(Ordering::Acquire)
    }

    /// Publishes a sticky owner-CPU reschedule request.
    pub fn request_reschedule(&self) {
        self.preempt_requested.store(true, Ordering::Release);
        self.need_resched.store(true, Ordering::Release);
    }

    pub(crate) fn request_scheduler_work(&self) {
        self.need_resched.store(true, Ordering::Release);
    }

    fn publish_deadline_work(&self) {
        self.deadline_work_pending.store(true, Ordering::Release);
        self.request_scheduler_work();
    }

    pub(crate) fn deadline_work_pending(&self) -> bool {
        self.deadline_work_pending.load(Ordering::Acquire)
    }

    fn begin_deadline_work(&self) -> bool {
        self.deadline_work_pending.swap(false, Ordering::AcqRel)
    }

    fn finish_deadline_work(&self, pending: bool) {
        // Only the owner CPU publishes deadline work, and both timer IRQ and
        // scheduler safe-point paths hold local IRQ exclusion while mutating
        // CpuLocal. The completed pass therefore owns the full publication
        // interval and may replace the sticky bit with its actual remainder.
        self.deadline_work_pending.store(pending, Ordering::Release);
        if pending {
            self.request_scheduler_work();
        }
    }

    pub(crate) fn kick_scheduler_work(&self) -> bool {
        self.request_scheduler_work();
        let Some(claim) = self.claim_scheduler_ipi() else {
            return false;
        };
        self.send_claimed_scheduler_ipi(claim);
        true
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
            self.request_reschedule();
        }
    }

    pub(crate) fn publish_remote_wake(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        if !self.is_online() {
            return PublishResult::WrongKind;
        }
        let _publication = IrqScope::enter();
        let (result, _head_became_non_empty) = self
            .remote_wake_inbox
            .publish_with_head_transition(node, message);
        if matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ) {
            self.kick_scheduler_work();
        }
        result
    }

    pub(crate) fn publish_policy_update(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        if !self.is_online() {
            return PublishResult::WrongKind;
        }
        let _publication = IrqScope::enter();
        let (result, _head_became_non_empty) = self
            .migration_inbox
            .publish_with_head_transition(node, message);
        if matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ) {
            self.kick_scheduler_work();
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
    pub fn acknowledge_scheduler_ipi(&self) {
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

    fn arm_scheduler_deadline(&self, deadline_ns: u64) {
        let mut current = self.scheduler_deadline_ns.load(Ordering::Acquire);
        loop {
            if current != 0 && current <= deadline_ns {
                return;
            }
            match self.scheduler_deadline_ns.compare_exchange_weak(
                current,
                deadline_ns,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    fn clear_due_deferred_deadline(&self, now_ns: u64) {
        let mut current = self.deferred_scheduler_deadline_ns.load(Ordering::Acquire);
        loop {
            if current == 0 || current > now_ns {
                return;
            }
            match self.deferred_scheduler_deadline_ns.compare_exchange_weak(
                current,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
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

/// Scheduler state that is created explicitly and mutated only by its owner CPU.
///
/// The object is `!Unpin`; runtimes store it in per-CPU pinned allocations and
/// publish it only after registration has completed.
#[derive(Debug)]
pub struct CpuLocal {
    owner: CpuId,
    remote: Arc<CpuRemote>,
    pub(crate) current: Option<ThreadId>,
    pub(crate) current_core: Option<Arc<ThreadCore>>,
    pub(crate) current_dispatch: Option<CurrentDispatch>,
    pub(crate) idle: Option<ThreadId>,
    pub(crate) idle_core: Option<Arc<ThreadCore>>,
    pub(crate) run_queue: RunQueue,
    /// Stable references to Deadline reservations whose GRUB/CBS state is
    /// owned by this CPU, including blocked non-contending reservations that
    /// are absent from both `current` and the runqueue.
    pub(crate) deadline_members: Vec<Arc<ThreadCore>>,
    pub(crate) rt_bandwidth: RtBandwidth,
    deadline_this_bw_scaled: u64,
    deadline_running_bw_scaled: u64,
    deadline_max_bw_scaled: u64,
    pub(crate) task_deadlines: TaskDeadlineQueue,
    pub(crate) remote_wake_buffer: Vec<InboxMessage>,
    pub(crate) migration_buffer: Vec<InboxMessage>,
    deadline_expired_buffer: Vec<ExpiredTaskDeadline>,
    deadline_expired_count: usize,
    task_deadline_generation: u64,
    deadline_scan_cursor: usize,
    fair_balance_interval_ns: u64,
    switch_handoff: Option<SwitchHandoff>,
    batch_limit: usize,
    _pinned: PhantomPinned,
}

impl CpuLocal {
    pub(crate) fn create(
        owner: CpuId,
        config: TaskSystemConfig,
        remote: Arc<CpuRemote>,
    ) -> Pin<Box<Self>> {
        debug_assert_eq!(owner, remote.owner());
        Box::pin(Self {
            owner,
            remote,
            current: None,
            current_core: None,
            current_dispatch: None,
            idle: None,
            idle_core: None,
            run_queue: RunQueue::new(),
            deadline_members: Vec::with_capacity(config.timer_capacity()),
            rt_bandwidth: RtBandwidth::new(config.rt_period_ns(), config.rt_runtime_ns()),
            deadline_this_bw_scaled: 0,
            deadline_running_bw_scaled: 0,
            deadline_max_bw_scaled: u64::from(config.deadline_cap_percent()) * 10_000_000,
            task_deadlines: TaskDeadlineQueue::new(config.timer_capacity()),
            remote_wake_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            migration_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            deadline_expired_buffer: vec![ExpiredTaskDeadline::EMPTY; config.batch_limit()],
            deadline_expired_count: 0,
            task_deadline_generation: 0,
            deadline_scan_cursor: 0,
            fair_balance_interval_ns: config.balance_interval_ns().max(1),
            switch_handoff: None,
            batch_limit: config.batch_limit(),
            _pinned: PhantomPinned,
        })
    }

    /// Returns the logical processor that exclusively owns the run queue.
    pub const fn owner(&self) -> CpuId {
        self.owner
    }

    /// Returns whether registration and online publication have completed.
    pub fn is_online(&self) -> bool {
        self.remote.is_online()
    }

    pub(crate) fn remote(&self) -> &Arc<CpuRemote> {
        &self.remote
    }

    /// Returns the currently executing non-idle thread, if any.
    pub const fn current(&self) -> Option<ThreadId> {
        self.current
    }

    pub(crate) fn current_core(&self) -> Option<&Arc<ThreadCore>> {
        self.current_core.as_ref()
    }

    /// Clones a strong handle for the currently executing thread.
    ///
    /// This owner-side lookup never consults the generation registry. The
    /// stable core retained by `CpuLocal` pins the registry record and any OS
    /// extension until the returned handle is dropped.
    pub fn current_thread_handle(&self) -> Result<ThreadHandle, TaskError> {
        self.current_core
            .as_ref()
            .map(|core| ThreadHandle::from_core(Arc::clone(core)))
            .ok_or(TaskError::NoRunnableThread)
    }

    /// Returns the configured CPU idle thread, if any.
    pub const fn idle(&self) -> Option<ThreadId> {
        self.idle
    }

    /// Returns the number of runnable non-idle threads.
    pub(crate) const fn runnable_count(&self) -> usize {
        self.run_queue.len()
    }

    /// Publishes a sticky reschedule request from task or IRQ context.
    pub fn request_reschedule(&self) {
        self.remote.request_reschedule();
    }

    pub(crate) fn request_scheduler_work(&self) {
        self.remote.request_scheduler_work();
    }

    /// Tests the sticky reschedule request without clearing it.
    pub fn needs_reschedule(&self) -> bool {
        self.remote.needs_reschedule()
    }

    /// Returns the preallocated task-deadline capacity selected at construction.
    pub fn timer_capacity(&self) -> usize {
        self.task_deadlines.capacity()
    }

    /// Returns the bounded scheduler safe-point work budget.
    pub const fn batch_limit(&self) -> usize {
        self.batch_limit
    }

    pub(crate) fn clear_current(self: Pin<&mut Self>) {
        let fields = self.fields_mut();
        fields.current = None;
        fields.current_core = None;
        fields.current_dispatch = None;
        fields.remote.publish_current_thread(None);
    }

    pub(crate) fn set_current_core(self: Pin<&mut Self>, core: Arc<ThreadCore>) {
        let id = core.id();
        let fields = self.fields_mut();
        fields.current = Some(id);
        fields.current_core = Some(core);
        fields.remote.publish_current_thread(Some(id));
        fields.remote.mark_scheduler_ready();
    }

    pub(crate) fn install_dispatch(self: Pin<&mut Self>, dispatch: CurrentDispatch) {
        // SAFETY: replacing copy-only owner state cannot move CpuLocal.
        unsafe { self.get_unchecked_mut() }.current_dispatch = Some(dispatch);
    }

    pub(crate) fn take_dispatch(self: Pin<&mut Self>) -> Option<CurrentDispatch> {
        // SAFETY: taking copy-only owner state cannot move CpuLocal.
        unsafe { self.get_unchecked_mut() }.current_dispatch.take()
    }

    /// Reads the lock-free lifecycle published by the current dispatch.
    pub(crate) fn current_lifecycle_state(&self) -> Option<ThreadState> {
        self.current_dispatch
            .as_ref()
            .map(|dispatch| dispatch.runtime_core().state())
    }

    pub(crate) fn charge_current_dispatch(
        self: Pin<&mut Self>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<DispatchCharge, TaskError> {
        let fields = self.fields_mut();
        let current_is_non_idle = fields.current.is_some() && fields.current != fields.idle;
        let grub_reclaimed_ns = fields.current_dispatch.as_ref().map_or(0, |dispatch| {
            dispatch.grub_reclaimed_ns(
                runtime_ns,
                fields
                    .deadline_this_bw_scaled
                    .saturating_sub(fields.deadline_running_bw_scaled),
                fields.deadline_max_bw_scaled,
            )
        });
        let dispatch = fields
            .current_dispatch
            .as_mut()
            .ok_or(TaskError::NoRunnableThread)?;
        if current_is_non_idle {
            fields.remote.charge_busy_runtime(runtime_ns);
        }
        let charge = dispatch.charge(
            runtime_ns,
            now_ns,
            reclaimed_ns.saturating_add(grub_reclaimed_ns),
        );
        let current_policy = dispatch.policy;
        let current_fair = dispatch.entity.fair();
        let rt_quota_exempt = dispatch.rt_quota_exempt;
        fields.run_queue.update_fair_virtual_time(current_fair);
        let rt_quota_exhausted = if matches!(
            current_policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) {
            fields.rt_bandwidth.charge(now_ns, runtime_ns)
        } else {
            false
        };
        if charge.slice_expired
            || charge.deadline_overrun
            || (rt_quota_exhausted && !rt_quota_exempt)
        {
            fields.request_reschedule();
        }
        fields.recompute_scheduler_deadline(now_ns);
        Ok(charge)
    }

    pub(crate) fn settle_current_dispatch(
        mut self: Pin<&mut Self>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<DispatchCharge, TaskError> {
        let runtime_ns = self
            .as_ref()
            .get_ref()
            .current_dispatch
            .as_ref()
            .ok_or(TaskError::NoRunnableThread)?
            .unaccounted_runtime(now_ns);
        self.as_mut()
            .charge_current_dispatch(now_ns, runtime_ns, reclaimed_ns)
    }

    pub(crate) fn set_idle(self: Pin<&mut Self>, idle: ThreadId, core: Arc<ThreadCore>) {
        debug_assert_eq!(idle, core.id());
        // SAFETY: changing fields does not move this pinned object.
        let fields = unsafe { self.get_unchecked_mut() };
        fields.idle = Some(idle);
        fields.idle_core = Some(core);
        fields.remote.publish_idle_thread(idle);
        fields.remote.mark_scheduler_ready();
    }

    pub(crate) fn stage_switch_handoff(
        self: Pin<&mut Self>,
        previous: Arc<ThreadCore>,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        let handoff = &mut self.fields_mut().switch_handoff;
        if handoff.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        *handoff = Some(SwitchHandoff {
            previous,
            migration_target,
            runtime_tail_finished: false,
        });
        Ok(())
    }

    pub(crate) fn finish_switch_runtime_tail(
        self: Pin<&mut Self>,
        previous: ThreadId,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        let handoff = self
            .fields_mut()
            .switch_handoff
            .as_mut()
            .ok_or(TaskError::InvalidConfiguration)?;
        if handoff.previous.id() != previous
            || handoff.migration_target != migration_target
            || handoff.runtime_tail_finished
        {
            return Err(TaskError::InvalidConfiguration);
        }
        handoff.runtime_tail_finished = true;
        Ok(())
    }

    pub(crate) fn take_switch_handoff(self: Pin<&mut Self>) -> Option<SwitchHandoff> {
        self.fields_mut().switch_handoff.take()
    }

    pub(crate) fn switch_handoff(&self) -> Option<&SwitchHandoff> {
        self.switch_handoff.as_ref()
    }

    pub(crate) fn register_deadline_member(
        &mut self,
        core: &Arc<ThreadCore>,
    ) -> Result<bool, TaskError> {
        if self
            .deadline_members
            .iter()
            .all(|member| !Arc::ptr_eq(member, core))
        {
            if self.deadline_members.len() == self.deadline_members.capacity() {
                return Err(TaskError::TimerCapacity);
            }
            self.deadline_members.push(Arc::clone(core));
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        if let Some(index) = self
            .deadline_members
            .iter()
            .position(|member| Arc::ptr_eq(member, core))
        {
            self.deadline_members.swap_remove(index);
            if self.deadline_members.is_empty() {
                self.deadline_scan_cursor = 0;
            } else {
                self.deadline_scan_cursor %= self.deadline_members.len();
            }
        }
    }

    pub(crate) fn scheduler_enter(self: Pin<&mut Self>) -> bool {
        // `need_resched` is cleared only after entering the scheduler, never by
        // wake, timer, IPI, or preemption-disable paths. The AcqRel claim pairs
        // with producer Release stores after inbox publication. Rechecking the
        // inbox after the claim closes the race where a forced scheduling path
        // otherwise overwrote a remote producer's doorbell.
        self.remote.scheduler_enter()
    }

    pub(crate) fn take_preempt_requested(&self) -> bool {
        self.remote.take_preempt_requested()
    }

    pub(crate) fn defer_park_preemption(&self, requested: bool) {
        self.remote.defer_park_preemption(requested);
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        self.remote.finish_park_preemption(resume_running);
    }

    pub(crate) fn fields_mut(self: Pin<&mut Self>) -> &mut Self {
        // SAFETY: the returned borrow cannot move the `!Unpin` object and is
        // bounded by the pinned mutable borrow.
        unsafe { self.get_unchecked_mut() }
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        self.remote.balance_request_node()
    }

    pub(crate) fn publish_load_summary(
        &self,
        current_key: Option<SchedulingKey>,
        pushable_key: Option<SchedulingKey>,
        runnable_count: usize,
        overloaded: bool,
    ) {
        self.remote
            .publish_load_summary(current_key, pushable_key, runnable_count, overloaded);
    }

    pub(crate) fn add_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let next_this_bw_scaled = self
            .deadline_this_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let next_running_bw_scaled = if active {
            self.deadline_running_bw_scaled
                .checked_add(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_this_bw_scaled = next_this_bw_scaled;
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn remove_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let next_this_bw_scaled = self
            .deadline_this_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let next_running_bw_scaled = if active {
            self.deadline_running_bw_scaled
                .checked_sub(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_this_bw_scaled = next_this_bw_scaled;
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn activate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        let next_running_bw_scaled = self
            .deadline_running_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        if next_running_bw_scaled > self.deadline_this_bw_scaled {
            return Err(TaskError::InvalidConfiguration);
        }
        self.deadline_running_bw_scaled = next_running_bw_scaled;
        Ok(())
    }

    pub(crate) fn deactivate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        self.deadline_running_bw_scaled = self
            .deadline_running_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(())
    }

    /// Returns the owner runqueue's GRUB bandwidth accounting.
    pub const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        DeadlineBandwidthSnapshot {
            this_bw_scaled: self.deadline_this_bw_scaled,
            running_bw_scaled: self.deadline_running_bw_scaled,
            max_bw_scaled: self.deadline_max_bw_scaled,
        }
    }

    pub(crate) fn arm_deferred_scheduler_deadline(&self, deadline_ns: u64) {
        if deadline_ns == 0 {
            return;
        }
        let mut current = self
            .remote
            .deferred_scheduler_deadline_ns
            .load(Ordering::Acquire);
        loop {
            if current != 0 && current <= deadline_ns {
                return;
            }
            match self
                .remote
                .deferred_scheduler_deadline_ns
                .compare_exchange_weak(current, deadline_ns, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.remote.arm_scheduler_deadline(deadline_ns);
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn replace_scheduler_deadline(&self, deadline_ns: Option<u64>) {
        self.remote
            .scheduler_deadline_ns
            .store(deadline_ns.unwrap_or(0), Ordering::Release);
    }

    pub(crate) fn take_due_scheduler_deadline(&self, now_ns: u64) -> bool {
        let mut current = self.remote.scheduler_deadline_ns.load(Ordering::Acquire);
        loop {
            if current == 0 || current > now_ns {
                return false;
            }
            match self.remote.scheduler_deadline_ns.compare_exchange_weak(
                current,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.remote.clear_due_deferred_deadline(now_ns);
                    return true;
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn scheduler_deadline_ns(&self) -> Option<u64> {
        let deadline_ns = self.remote.scheduler_deadline_ns.load(Ordering::Acquire);
        (deadline_ns != 0).then_some(deadline_ns)
    }

    pub(crate) fn refresh_scheduler_deadline(self: Pin<&mut Self>, now_ns: u64) {
        self.fields_mut().recompute_scheduler_deadline(now_ns);
    }

    pub(crate) fn next_oneshot_deadline_ns(
        &self,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Option<u64> {
        let deferred_timer_backlog = self.remote.deadline_work_pending()
            && self.task_deadlines.has_immediately_actionable_entry(now_ns);
        let timer = if deferred_timer_backlog {
            // A bounded hard-IRQ pass already published sticky owner work and
            // need_resched. Re-arming the overdue heap head at the hardware
            // resolution would create an interrupt storm that can prevent the
            // scheduler safe point from draining that work. Keep future
            // scheduler deadlines visible and let the runtime's periodic
            // source remain the failsafe clockevent.
            None
        } else {
            self.task_deadlines
                .next_deadline_ns(now_ns, timer_resolution_ns)
        };
        let earliest_future_ns = now_ns
            .checked_add(timer_resolution_ns.max(1))
            .or_else(|| now_ns.checked_add(1));
        let scheduler = match self.scheduler_deadline_ns() {
            Some(deadline) if deadline <= now_ns => {
                // Linux does not start a scheduler hrtimer whose expiry has
                // already passed: the owning runqueue handles that state
                // immediately. Preserve the same boundary here. Consuming the
                // atomic deadline also clears any matching deferred owner
                // event; sticky work then forces a scheduler safe point
                // without manufacturing a resolution-rate interrupt loop.
                if self.take_due_scheduler_deadline(now_ns) {
                    self.request_scheduler_work();
                }
                None
            }
            Some(deadline) => earliest_future_ns.map(|earliest| deadline.max(earliest)),
            None => None,
        };
        match (timer, scheduler) {
            (Some(timer), Some(scheduler)) => Some(timer.min(scheduler)),
            (Some(timer), None) => Some(timer),
            (None, Some(scheduler)) => Some(scheduler),
            (None, None) => None,
        }
    }

    pub(crate) fn next_task_deadline_update(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> Result<TaskDeadlineUpdate, TaskError> {
        let deadline = self
            .as_ref()
            .next_oneshot_deadline_ns(now_ns, timer_resolution_ns)
            .and_then(MonotonicDeadline::from_nanos);
        let fields = self.fields_mut();
        fields.task_deadline_generation = fields
            .task_deadline_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        TaskDeadlineUpdate::try_new(
            fields.task_deadline_generation,
            deadline,
            fields.remote.deadline_work_pending(),
        )
        .ok_or(TaskError::InvalidConfiguration)
    }

    #[cfg(test)]
    pub(crate) fn set_task_deadline_generation_for_test(self: Pin<&mut Self>, generation: u64) {
        self.fields_mut().task_deadline_generation = generation;
    }

    fn recompute_scheduler_deadline(&mut self, now_ns: u64) {
        let mut next_deadline_ns = nonzero_deadline(
            self.remote
                .deferred_scheduler_deadline_ns
                .load(Ordering::Acquire),
        );
        if let Some(deadline) = self.run_queue.earliest_deadline_event_ns() {
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }

        let current_is_idle = self.current.is_some() && self.current == self.idle;
        if !current_is_idle && let Some(dispatch) = self.current_dispatch.as_ref() {
            let fair_slice_required = dispatch.entity.fair().is_none_or(|fair| {
                if fair.mode() == FairMode::Idle {
                    self.run_queue.has_idle_fair()
                } else {
                    self.run_queue.has_fair()
                }
            });
            if fair_slice_required && let Some(deadline) = dispatch.next_scheduler_event_ns(now_ns)
            {
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
            if dispatch.is_rt() && !dispatch.rt_quota_exempt {
                let remaining = self.rt_bandwidth.remaining_runtime_ns(now_ns);
                let deadline = if remaining == 0 {
                    self.rt_bandwidth.next_period_ns(now_ns)
                } else {
                    now_ns.saturating_add(remaining)
                };
                next_deadline_ns = earliest(next_deadline_ns, deadline);
            }
        }
        if self.run_queue.has_rt() && self.rt_bandwidth.is_throttled(now_ns) {
            let deadline = self.rt_bandwidth.next_period_ns(now_ns);
            next_deadline_ns = earliest(next_deadline_ns, deadline);
        }
        let current_non_idle = self.current.is_some() && self.current != self.idle;
        if self.run_queue.has_fair()
            && self
                .run_queue
                .len()
                .saturating_add(usize::from(current_non_idle))
                > 1
        {
            next_deadline_ns = earliest(
                next_deadline_ns,
                self.remote.fair_balance_deadline_ns.load(Ordering::Acquire),
            );
        }
        self.replace_scheduler_deadline(next_deadline_ns);
    }

    pub(crate) const fn deadline_scan_cursor(&self) -> usize {
        self.deadline_scan_cursor
    }

    pub(crate) fn set_deadline_scan_cursor(&mut self, cursor: usize) {
        self.deadline_scan_cursor = cursor;
    }

    /// Attempts to return a coherent remotely observable scheduling snapshot.
    pub fn try_load_summary(&self) -> Option<CpuLoadSummary> {
        self.remote.try_load_summary()
    }

    /// Attempts to return the remotely observable queued runnable count.
    pub fn try_runnable_summary(&self) -> Option<usize> {
        self.remote.try_runnable_summary()
    }

    pub(crate) fn fair_balance_due(&self, now_ns: u64) -> bool {
        self.remote.fair_balance_due(now_ns)
    }

    pub(crate) fn reset_fair_balance(self: Pin<&mut Self>, now_ns: u64, minimum_interval_ns: u64) {
        let fields = self.fields_mut();
        let interval_ns = minimum_interval_ns.max(1);
        fields.fair_balance_interval_ns = interval_ns;
        fields.remote.defer_fair_balance(now_ns, interval_ns);
    }

    pub(crate) fn backoff_fair_balance(
        self: Pin<&mut Self>,
        now_ns: u64,
        minimum_interval_ns: u64,
        maximum_interval_ns: u64,
    ) {
        let fields = self.fields_mut();
        let minimum_interval_ns = minimum_interval_ns.max(1);
        let maximum_interval_ns = maximum_interval_ns.max(minimum_interval_ns);
        let current_interval_ns = fields
            .fair_balance_interval_ns
            .clamp(minimum_interval_ns, maximum_interval_ns);
        let next_interval_ns = current_interval_ns
            .saturating_mul(2)
            .min(maximum_interval_ns);
        fields.fair_balance_interval_ns = next_interval_ns;
        fields.remote.defer_fair_balance(now_ns, next_interval_ns);
    }

    /// Returns mutable owner-only access to the preallocated task-deadline heap.
    pub fn task_deadlines(self: Pin<&mut Self>) -> &mut TaskDeadlineQueue {
        // SAFETY: the pinned mutable owner borrow excludes every concurrent
        // timer consumer and does not move CpuLocal or its heap.
        &mut unsafe { self.get_unchecked_mut() }.task_deadlines
    }

    /// Expires one bounded timer batch without allocation or callbacks.
    pub fn expire_task_deadlines(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
        budget: usize,
    ) -> TaskDeadlineExpireBatch {
        let fields = self.fields_mut();
        let available = fields
            .deadline_expired_buffer
            .len()
            .saturating_sub(fields.deadline_expired_count);
        let request = TaskDeadlineExpireRequest::new(
            now_ns,
            budget.min(fields.batch_limit).min(available),
            timer_resolution_ns,
        );
        let output = &mut fields.deadline_expired_buffer[fields.deadline_expired_count..];
        let batch = fields.task_deadlines.expire(request, output);
        fields.deadline_expired_count += batch.expired();
        if batch.pending() || batch.expired() != 0 {
            fields.remote.publish_deadline_work();
        }
        batch
    }

    pub(crate) fn begin_deadline_work(self: Pin<&mut Self>) -> bool {
        self.remote.begin_deadline_work()
    }

    pub(crate) fn finish_deadline_work(self: Pin<&mut Self>, pending: bool) {
        self.remote.finish_deadline_work(pending);
    }

    /// Copies expired timer events to task-context storage.
    ///
    /// Events that do not fit in `output` remain buffered for the next
    /// task-context drain.
    pub fn take_expired_task_deadlines(
        self: Pin<&mut Self>,
        output: &mut [ExpiredTaskDeadline],
    ) -> usize {
        let fields = self.fields_mut();
        let buffered = fields.deadline_expired_count;
        let count = buffered.min(output.len());
        output[..count].copy_from_slice(&fields.deadline_expired_buffer[..count]);
        let remaining = buffered - count;
        fields
            .deadline_expired_buffer
            .copy_within(count..buffered, 0);
        fields.deadline_expired_buffer[remaining..buffered].fill(ExpiredTaskDeadline::EMPTY);
        fields.deadline_expired_count = remaining;
        count
    }

    pub(crate) fn take_expired_task_deadline(self: Pin<&mut Self>) -> Option<ExpiredTaskDeadline> {
        let fields = self.fields_mut();
        let index = fields.deadline_expired_buffer[..fields.deadline_expired_count]
            .iter()
            .rposition(|event| event.thread().is_some())?;
        fields.deadline_expired_count -= 1;
        let last = fields.deadline_expired_count;
        fields.deadline_expired_buffer.swap(index, last);
        Some(core::mem::replace(
            &mut fields.deadline_expired_buffer[last],
            ExpiredTaskDeadline::EMPTY,
        ))
    }

    /// Returns the migration publication endpoint for remote CPUs.
    pub fn migration_inbox(&self) -> &SchedulerInbox {
        self.remote.migration_inbox()
    }

    /// Returns the deferred-reclaim publication endpoint for remote CPUs.
    pub fn reclaim_inbox(&self) -> &SchedulerInbox {
        self.remote.reclaim_inbox()
    }

    /// Reports pending remote work before idle or scheduler exit.
    pub fn has_remote_work(&self) -> bool {
        self.remote.has_remote_work()
    }

    /// Acknowledges one coalesced scheduler IPI epoch and rechecks publication.
    pub fn acknowledge_scheduler_ipi(&self) {
        self.remote.acknowledge_scheduler_ipi();
    }

    /// Publishes the idle/polling state and performs the final WFI recheck.
    pub fn prepare_idle_wait(&self) -> bool {
        self.remote.prepare_idle_wait()
    }

    /// Clears idle/polling publication after WFI returns.
    pub fn finish_idle_wait(&self) {
        self.remote.finish_idle_wait();
    }

    /// Returns whether this CPU is between idle publication and WFI completion.
    pub fn is_idle_polling(&self) -> bool {
        self.remote.is_idle_polling()
    }
}

fn nonzero_deadline(deadline_ns: u64) -> Option<u64> {
    (deadline_ns != 0).then_some(deadline_ns)
}

fn earliest(current: Option<u64>, candidate: u64) -> Option<u64> {
    Some(current.map_or(candidate, |current| current.min(candidate)))
}

#[cfg(test)]
mod scheduler_ipi_tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn overdue_scheduler_deadline_becomes_sticky_work_instead_of_a_resolution_timer() {
        let remote = CpuRemote::create(CpuId::new(0));
        let cpu = CpuLocal::create(CpuId::new(0), TaskSystemConfig::new(1), Arc::clone(&remote));
        cpu.arm_deferred_scheduler_deadline(100);

        assert_eq!(
            cpu.next_oneshot_deadline_ns(100, 1),
            None,
            "an overdue scheduler event must not be rearmed at timer resolution"
        );
        assert_eq!(cpu.scheduler_deadline_ns(), None);
        assert!(
            remote.needs_reschedule(),
            "the consumed deadline must remain visible as scheduler work"
        );
    }

    #[test]
    fn load_summary_reader_does_not_wait_for_stalled_writer() {
        let remote = CpuRemote::create(CpuId::new(0));
        remote.load_summary_sequence.store(1, Ordering::Release);

        let reader_remote = Arc::clone(&remote);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let summary = reader_remote.try_load_summary();
            finished_tx.send(summary).unwrap();
        });

        started_rx.recv().unwrap();
        let completed_while_writer_stalled =
            finished_rx.recv_timeout(Duration::from_millis(100)).is_ok();

        // Always release the old implementation's unbounded reader so a red
        // result cannot leak a host thread or hang the test process.
        remote.load_summary_sequence.store(2, Ordering::Release);
        reader.join().unwrap();

        assert!(
            completed_while_writer_stalled,
            "remote balancing must not spin forever behind a stalled owner writer"
        );
    }

    #[test]
    fn stale_coalesced_completion_cannot_clear_a_newer_doorbell_epoch() {
        let remote = CpuRemote::create(CpuId::new(0));
        let old = remote.claim_scheduler_ipi().unwrap();

        // A safe point may consume the old reason before its transport call
        // reports that an older physical delivery covers it. A later producer
        // can then own a new epoch, which the stale completion must not clear.
        remote.acknowledge_scheduler_ipi();
        let new = remote.claim_scheduler_ipi().unwrap();
        remote.finish_scheduler_ipi_send(old, RuntimeStatus::Busy);

        assert_eq!(remote.scheduler_ipi_pending.load(Ordering::Acquire), new.0);
        assert_ne!(new.0 & IPI_CLAIMED, 0);
    }

    #[test]
    fn coalesced_scheduler_ipi_keeps_the_inflight_delivery_claimed() {
        let remote = CpuRemote::create(CpuId::new(0));
        remote.request_scheduler_work();
        let claim = remote.claim_scheduler_ipi().unwrap();

        remote.finish_scheduler_ipi_send(claim, RuntimeStatus::Busy);

        assert_eq!(
            remote.scheduler_ipi_pending.load(Ordering::Acquire),
            claim.0,
            "Busy means an older physical delivery covers this coalesced epoch"
        );
    }
}

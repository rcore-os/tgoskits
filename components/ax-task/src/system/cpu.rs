//! Pinned owner-CPU scheduler state.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    marker::{PhantomData, PhantomPinned},
    ops::Deref,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    CpuId, DeadlineAdmission, FairMode, RtBandwidth, RunQueue, SchedulePolicy, SchedulingEntity,
    SchedulingKey, TaskError, TaskSystemConfig, ThreadHandle, ThreadId, ThreadState,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    runtime::{RuntimeCpuId, RuntimeStatus, task_runtime},
    thread::ThreadCore,
    timer::{ExpireBatch, ExpireRequest, ExpiredTimer, TimerQueue},
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
const IPI_RETRY_WORD_BITS: usize = u64::BITS as usize;
const IPI_CLAIMED: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchedulerIpiClaim(u64);

/// Preallocated cross-CPU retry/quarantine publication for scheduler IPIs.
#[derive(Debug)]
pub(crate) struct SchedulerIpiRetrySet {
    retry_words: Box<[AtomicU64]>,
    invalid_words: Box<[AtomicU64]>,
    retry_summary: AtomicBool,
    invalid_summary: AtomicBool,
}

impl SchedulerIpiRetrySet {
    pub(crate) fn new(cpu_count: usize) -> Self {
        let word_count = cpu_count.div_ceil(IPI_RETRY_WORD_BITS);
        Self {
            retry_words: (0..word_count)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            invalid_words: (0..word_count)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            retry_summary: AtomicBool::new(false),
            invalid_summary: AtomicBool::new(false),
        }
    }

    fn publish(words: &[AtomicU64], summary: &AtomicBool, cpu: CpuId) {
        let index = cpu.as_u32() as usize;
        let word = index / IPI_RETRY_WORD_BITS;
        let bit = index % IPI_RETRY_WORD_BITS;
        let Some(slot) = words.get(word) else {
            return;
        };
        slot.fetch_or(1_u64 << bit, Ordering::Release);
        summary.store(true, Ordering::Release);
    }

    pub(crate) fn publish_retry(&self, cpu: CpuId) {
        Self::publish(&self.retry_words, &self.retry_summary, cpu);
    }

    pub(crate) fn publish_invalid(&self, cpu: CpuId) {
        Self::publish(&self.invalid_words, &self.invalid_summary, cpu);
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.retry_summary.load(Ordering::Acquire) || self.invalid_summary.load(Ordering::Acquire)
    }

    pub(crate) fn take_retry_batch(&self, output: &mut [CpuId]) -> usize {
        Self::take_batch(&self.retry_words, &self.retry_summary, output)
    }

    pub(crate) fn take_invalid_batch(&self, output: &mut [CpuId]) -> usize {
        Self::take_batch(&self.invalid_words, &self.invalid_summary, output)
    }

    fn take_batch(words: &[AtomicU64], summary: &AtomicBool, output: &mut [CpuId]) -> usize {
        if !summary.swap(false, Ordering::AcqRel) {
            return 0;
        }

        let mut count = 0;
        for (word_index, word) in words.iter().enumerate() {
            let mut pending = word.swap(0, Ordering::AcqRel);
            while pending != 0 {
                if count == output.len() {
                    word.fetch_or(pending, Ordering::Release);
                    summary.store(true, Ordering::Release);
                    return count;
                }
                let bit = pending.trailing_zeros() as usize;
                pending &= pending - 1;
                output[count] = CpuId::new((word_index * IPI_RETRY_WORD_BITS + bit) as u32);
                count += 1;
            }
        }
        count
    }
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
    online: AtomicBool,
    scheduler_ready: AtomicBool,
    need_resched: AtomicBool,
    preempt_requested: AtomicBool,
    park_preempt_deferred: AtomicBool,
    scheduler_ipi_pending: AtomicU64,
    scheduler_ipi_fault_count: AtomicU64,
    scheduler_ipi_retries: Arc<SchedulerIpiRetrySet>,
    idle_polling: AtomicBool,
    current_thread: AtomicU64,
    idle_thread: AtomicU64,
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
    pub(crate) fn create(
        owner: CpuId,
        config: TaskSystemConfig,
        scheduler_ipi_retries: Arc<SchedulerIpiRetrySet>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            owner_claimed: AtomicBool::new(false),
            online: AtomicBool::new(false),
            scheduler_ready: AtomicBool::new(false),
            need_resched: AtomicBool::new(false),
            preempt_requested: AtomicBool::new(false),
            park_preempt_deferred: AtomicBool::new(false),
            scheduler_ipi_pending: AtomicU64::new(0),
            scheduler_ipi_fault_count: AtomicU64::new(0),
            scheduler_ipi_retries,
            idle_polling: AtomicBool::new(false),
            current_thread: AtomicU64::new(0),
            idle_thread: AtomicU64::new(0),
            load_summary_sequence: AtomicU64::new(0),
            load_summary_runnable: AtomicUsize::new(0),
            load_summary_flags: AtomicU8::new(0),
            load_summary_current_primary: AtomicU64::new(0),
            load_summary_current_sequence: AtomicU64::new(0),
            load_summary_pushable_primary: AtomicU64::new(0),
            load_summary_pushable_sequence: AtomicU64::new(0),
            fair_balance_deadline_ns: AtomicU64::new(config.balance_interval_ns()),
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

    fn finish_scheduler_ipi_send(&self, claim: SchedulerIpiClaim, status: RuntimeStatus) {
        match status {
            RuntimeStatus::Success => {}
            RuntimeStatus::Busy => {
                self.scheduler_ipi_fault_count
                    .fetch_add(1, Ordering::Relaxed);
                let _ = self.scheduler_ipi_pending.compare_exchange(
                    claim.0,
                    claim.0 & !IPI_CLAIMED,
                    Ordering::Release,
                    Ordering::Acquire,
                );
                core::sync::atomic::fence(Ordering::SeqCst);
                if self.has_scheduler_work()
                    && self.scheduler_ipi_pending.load(Ordering::Acquire) & IPI_CLAIMED == 0
                {
                    self.scheduler_ipi_retries.publish_retry(self.owner);
                }
            }
            _ => {
                self.scheduler_ipi_fault_count
                    .fetch_add(1, Ordering::Relaxed);
                let _ = self.scheduler_ipi_pending.compare_exchange(
                    claim.0,
                    claim.0 & !IPI_CLAIMED,
                    Ordering::Release,
                    Ordering::Acquire,
                );
                core::sync::atomic::fence(Ordering::SeqCst);
                self.scheduler_ipi_retries.publish_invalid(self.owner);
            }
        }
    }

    /// Completes one already-claimed doorbell transaction and always feeds the
    /// typed transport result back into the coalescing/retry state machine.
    fn send_claimed_scheduler_ipi(&self, claim: SchedulerIpiClaim) {
        let status = task_runtime::send_scheduler_ipi(RuntimeCpuId::new(self.owner.as_u32()));
        self.finish_scheduler_ipi_send(claim, status);
    }

    pub(crate) fn retry_scheduler_ipi(&self) -> bool {
        if !self.has_scheduler_work() {
            return false;
        }
        let Some(claim) = self.claim_scheduler_ipi() else {
            return false;
        };
        self.send_claimed_scheduler_ipi(claim);
        true
    }

    /// Returns failed scheduler-doorbell transactions observed for this CPU.
    pub fn scheduler_ipi_fault_count(&self) -> u64 {
        self.scheduler_ipi_fault_count.load(Ordering::Acquire)
    }

    fn has_scheduler_work(&self) -> bool {
        self.needs_reschedule() || self.has_remote_work()
    }

    /// Tests the sticky reschedule request without consuming it.
    pub fn needs_reschedule(&self) -> bool {
        self.need_resched.load(Ordering::Acquire)
    }

    pub(crate) fn scheduler_enter(&self) -> bool {
        self.need_resched.swap(false, Ordering::AcqRel);
        let preempt_requested = self.preempt_requested.swap(false, Ordering::AcqRel);
        if self.has_remote_work() {
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

    /// Returns a coherent remotely observable scheduling-load snapshot.
    pub fn load_summary(&self) -> CpuLoadSummary {
        loop {
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
            return CpuLoadSummary {
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
            };
        }
    }

    /// Returns the remotely observable queued runnable count.
    pub fn runnable_summary(&self) -> usize {
        self.load_summary().runnable_count()
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
        if self.has_remote_work() {
            self.request_scheduler_work();
        }
    }

    pub(crate) fn prepare_idle_wait(&self) -> bool {
        self.idle_polling.store(true, Ordering::Release);
        core::sync::atomic::fence(Ordering::SeqCst);
        let may_wait =
            !self.needs_reschedule() && !self.has_remote_work() && self.runnable_summary() == 0;
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
    pub(crate) timer_queue: TimerQueue,
    pub(crate) remote_wake_buffer: Vec<InboxMessage>,
    pub(crate) migration_buffer: Vec<InboxMessage>,
    timer_expired_buffer: Vec<ExpiredTimer>,
    timer_expired_count: usize,
    deadline_scan_cursor: usize,
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
            timer_queue: TimerQueue::new(config.timer_capacity()),
            remote_wake_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            migration_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            timer_expired_buffer: vec![ExpiredTimer::EMPTY; config.batch_limit()],
            timer_expired_count: 0,
            deadline_scan_cursor: 0,
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

    /// Returns the preallocated timer capacity selected at construction.
    pub fn timer_capacity(&self) -> usize {
        self.timer_queue.capacity()
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
        });
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
        let timer = self
            .timer_queue
            .next_deadline_ns(now_ns, timer_resolution_ns);
        let earliest_future_ns = now_ns
            .checked_add(timer_resolution_ns.max(1))
            .or_else(|| now_ns.checked_add(1));
        let scheduler = self
            .scheduler_deadline_ns()
            .and_then(|deadline| earliest_future_ns.map(|earliest| deadline.max(earliest)));
        match (timer, scheduler) {
            (Some(timer), Some(scheduler)) => Some(timer.min(scheduler)),
            (Some(timer), None) => Some(timer),
            (None, Some(scheduler)) => Some(scheduler),
            (None, None) => None,
        }
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
            if let Some(deadline) = dispatch.next_scheduler_event_ns(now_ns) {
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

    /// Returns a coherent remotely observable scheduling-load snapshot.
    pub fn load_summary(&self) -> CpuLoadSummary {
        self.remote.load_summary()
    }

    /// Returns the remotely observable queued runnable count.
    pub fn runnable_summary(&self) -> usize {
        self.remote.runnable_summary()
    }

    pub(crate) fn fair_balance_due(&self, now_ns: u64) -> bool {
        self.remote.fair_balance_due(now_ns)
    }

    pub(crate) fn defer_fair_balance(&self, now_ns: u64, interval_ns: u64) {
        self.remote.defer_fair_balance(now_ns, interval_ns);
    }

    /// Returns mutable owner-only access to the preallocated timer heap.
    pub fn timer_queue(self: Pin<&mut Self>) -> &mut TimerQueue {
        // SAFETY: the pinned mutable owner borrow excludes every concurrent
        // timer consumer and does not move CpuLocal or its heap.
        &mut unsafe { self.get_unchecked_mut() }.timer_queue
    }

    /// Expires one bounded timer batch without allocation or callbacks.
    pub fn expire_timers(
        self: Pin<&mut Self>,
        now_ns: u64,
        timer_resolution_ns: u64,
    ) -> ExpireBatch {
        let fields = self.fields_mut();
        let request = ExpireRequest::new(now_ns, fields.batch_limit, timer_resolution_ns);
        let output = &mut fields.timer_expired_buffer[fields.timer_expired_count..];
        let batch = fields.timer_queue.expire(request, output);
        fields.timer_expired_count += batch.expired();
        if batch.pending() || batch.expired() != 0 {
            fields.request_scheduler_work();
        }
        batch
    }

    /// Copies expired timer events to task-context storage and clears the batch.
    pub fn take_expired_timers(self: Pin<&mut Self>, output: &mut [ExpiredTimer]) -> usize {
        let fields = self.fields_mut();
        let count = fields.timer_expired_count.min(output.len());
        output[..count].copy_from_slice(&fields.timer_expired_buffer[..count]);
        fields.timer_expired_count = 0;
        count
    }

    pub(crate) fn take_thread_expired_timer(self: Pin<&mut Self>) -> Option<ExpiredTimer> {
        let fields = self.fields_mut();
        let index = fields.timer_expired_buffer[..fields.timer_expired_count]
            .iter()
            .rposition(|event| event.owner_thread().is_some())?;
        fields.timer_expired_count -= 1;
        let last = fields.timer_expired_count;
        fields.timer_expired_buffer.swap(index, last);
        Some(core::mem::replace(
            &mut fields.timer_expired_buffer[last],
            ExpiredTimer::EMPTY,
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

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Clone, Debug)]
pub(crate) struct SwitchHandoff {
    pub(crate) previous: Arc<ThreadCore>,
    pub(crate) migration_target: Option<CpuId>,
}

#[cfg(test)]
mod scheduler_ipi_tests {
    use super::*;

    #[test]
    fn stale_failure_cannot_clear_a_newer_doorbell_epoch() {
        let retries = Arc::new(SchedulerIpiRetrySet::new(1));
        let remote = CpuRemote::create(CpuId::new(0), TaskSystemConfig::new(1), retries);
        let old = remote.claim_scheduler_ipi().unwrap();

        // A safe point may consume the old reason before its transport call
        // reports a conservative Retry. A later producer can then own a new
        // epoch, which the stale completion must not clear.
        remote.acknowledge_scheduler_ipi();
        let new = remote.claim_scheduler_ipi().unwrap();
        remote.finish_scheduler_ipi_send(old, RuntimeStatus::Busy);

        assert_eq!(remote.scheduler_ipi_pending.load(Ordering::Acquire), new.0);
        assert_ne!(new.0 & IPI_CLAIMED, 0);
    }
}

/// Owner-CPU copy of the running thread's mutable dispatch accounting.
///
/// Timer IRQ mutates only this object. The scheduler commits it to the registry
/// at the next safe point, so hard IRQ never acquires the global task-system lock.
#[derive(Debug)]
pub(crate) struct CurrentDispatch {
    pub(crate) thread: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) deadline_donor: Option<ThreadId>,
    pub(crate) blocks_pi_waiter: bool,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) pi_critical_rescue: bool,
    pub(crate) policy_generation: u64,
    pub(crate) deadline_overrun: bool,
    runtime_core: Arc<ThreadCore>,
    deadline_donor_core: Option<Arc<ThreadCore>>,
    deadline_cbs_generation: Option<u64>,
    accounted_until_ns: u64,
    charged_runtime_ns: u64,
}

/// Registry state copied into one owner-CPU dispatch interval.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentDispatchState {
    pub(crate) thread: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) deadline_donor: Option<ThreadId>,
    pub(crate) blocks_pi_waiter: bool,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) pi_critical_rescue: bool,
    pub(crate) policy_generation: u64,
}

impl CurrentDispatch {
    pub(crate) fn new(
        state: CurrentDispatchState,
        runtime_core: &Arc<ThreadCore>,
        now_ns: u64,
    ) -> Self {
        runtime_core.begin_runtime_accounting(now_ns);
        Self {
            thread: state.thread,
            policy: state.policy,
            entity: state.entity,
            deadline_donor: state.deadline_donor,
            blocks_pi_waiter: state.blocks_pi_waiter,
            rt_quota_exempt: state.rt_quota_exempt,
            pi_critical_rescue: state.pi_critical_rescue,
            policy_generation: state.policy_generation,
            deadline_overrun: false,
            runtime_core: Arc::clone(runtime_core),
            deadline_donor_core: None,
            deadline_cbs_generation: None,
            accounted_until_ns: now_ns,
            charged_runtime_ns: 0,
        }
    }

    pub(crate) fn with_deadline_donor_core(
        mut self,
        donor: Option<Arc<ThreadCore>>,
        cbs_generation: Option<u64>,
    ) -> Self {
        debug_assert_eq!(self.deadline_donor.is_some(), donor.is_some());
        debug_assert!(cbs_generation.is_none() || donor.is_some());
        self.deadline_donor_core = donor;
        self.deadline_cbs_generation = cbs_generation;
        self
    }

    pub(crate) fn deadline_donor_core(&self) -> Option<&Arc<ThreadCore>> {
        self.deadline_donor_core.as_ref()
    }

    pub(crate) const fn deadline_cbs_generation(&self) -> Option<u64> {
        self.deadline_cbs_generation
    }

    fn charge(&mut self, runtime_ns: u64, now_ns: u64, reclaimed_ns: u64) -> DispatchCharge {
        self.charged_runtime_ns = self.charged_runtime_ns.saturating_add(runtime_ns);
        self.accounted_until_ns = now_ns;
        self.runtime_core().charge_runtime(runtime_ns, now_ns);
        if self.pi_critical_rescue {
            return DispatchCharge::default();
        }
        let mut slice_expired = self.entity.charge(runtime_ns, 0, reclaimed_ns);
        let mut deadline_overrun = false;
        if slice_expired && let SchedulePolicy::Deadline(policy) = self.policy {
            deadline_overrun = policy.flags().contains(crate::DeadlineFlags::DL_OVERRUN);
            self.deadline_overrun |= deadline_overrun;
            if self.blocks_pi_waiter {
                self.pi_critical_rescue = true;
                self.entity.enter_pi_critical_rescue();
                slice_expired = false;
            }
        }
        DispatchCharge {
            slice_expired,
            deadline_overrun,
        }
    }

    pub(crate) fn finish_runtime_accounting(&self, now_ns: u64) {
        self.runtime_core().finish_runtime_accounting(now_ns);
    }

    pub(crate) const fn charged_runtime_ns(&self) -> u64 {
        self.charged_runtime_ns
    }

    fn unaccounted_runtime(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.accounted_until_ns)
    }

    fn runtime_core(&self) -> &ThreadCore {
        &self.runtime_core
    }

    pub(crate) fn runtime_core_arc(&self) -> &Arc<ThreadCore> {
        &self.runtime_core
    }

    fn grub_reclaimed_ns(
        &self,
        runtime_ns: u64,
        inactive_bw_scaled: u64,
        max_bw_scaled: u64,
    ) -> u64 {
        // A PI owner may execute on a different CPU from the Deadline donor.
        // Its local GRUB snapshot therefore does not describe the donor's root
        // domain. Conservatively debit wall time until a coherent root-domain
        // bandwidth snapshot can be passed with the CBS baton.
        if self.deadline_donor.is_some() {
            return 0;
        }
        let SchedulePolicy::Deadline(policy) = self.policy else {
            return 0;
        };
        if !policy.flags().contains(crate::DeadlineFlags::RECLAIM) || max_bw_scaled == 0 {
            return 0;
        }
        let own_bw_scaled = u64::try_from(DeadlineAdmission::utilization(policy))
            .unwrap_or(u64::MAX)
            .min(max_bw_scaled);
        let charge_rate_scaled =
            own_bw_scaled.max(max_bw_scaled.saturating_sub(inactive_bw_scaled.min(max_bw_scaled)));
        let charged_ns = (runtime_ns as u128)
            .saturating_mul(charge_rate_scaled as u128)
            .saturating_add(max_bw_scaled as u128 - 1)
            / max_bw_scaled as u128;
        runtime_ns.saturating_sub(u64::try_from(charged_ns).unwrap_or(u64::MAX))
    }

    fn is_rt(&self) -> bool {
        matches!(
            self.policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        )
    }

    pub(crate) const fn scheduling_key(&self) -> SchedulingKey {
        match self.entity {
            SchedulingEntity::Fair(fair) => SchedulingKey::new(
                self.policy.class_rank(),
                fair.virtual_deadline(),
                self.thread.as_u64(),
            ),
            _ => self
                .entity
                .scheduling_key(self.policy, self.thread.as_u64()),
        }
    }

    pub(crate) const fn schedule_policy(&self) -> SchedulePolicy {
        self.policy
    }

    fn next_scheduler_event_ns(&self, now_ns: u64) -> Option<u64> {
        match self.entity {
            SchedulingEntity::Fair(fair) => {
                Some(now_ns.saturating_add(fair.remaining_request_ns()))
            }
            SchedulingEntity::Fifo => None,
            SchedulingEntity::RoundRobin {
                remaining_quantum_ns,
            } => Some(now_ns.saturating_add(remaining_quantum_ns)),
            SchedulingEntity::Deadline(deadline) => {
                let mut next = nonzero_deadline(deadline.next_scheduler_event_ns());
                if !self.pi_critical_rescue {
                    next = earliest(next, now_ns.saturating_add(deadline.remaining_runtime_ns()));
                }
                next
            }
        }
    }

    pub(crate) fn should_preempt(
        &self,
        woken_policy: SchedulePolicy,
        woken_entity: SchedulingEntity,
        fair_virtual_time: u64,
        wakeup_granularity_ns: u64,
    ) -> bool {
        match woken_policy {
            SchedulePolicy::Deadline(_) => match self.policy {
                SchedulePolicy::Deadline(_) => {
                    deadline_key(woken_entity) < deadline_key(self.entity)
                }
                _ => true,
            },
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                match self.policy {
                    SchedulePolicy::Deadline(_) => false,
                    SchedulePolicy::Fifo { priority: current }
                    | SchedulePolicy::RoundRobin {
                        priority: current, ..
                    } => priority > current,
                    SchedulePolicy::Fair { .. } => true,
                }
            }
            SchedulePolicy::Fair {
                mode: woken_mode, ..
            } => match self.policy {
                SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. } => false,
                SchedulePolicy::Fair {
                    mode: current_mode, ..
                } => {
                    if woken_mode == FairMode::Idle && current_mode != FairMode::Idle {
                        false
                    } else if woken_mode != FairMode::Idle && current_mode == FairMode::Idle {
                        true
                    } else if woken_mode == FairMode::Batch {
                        // Batch suppresses ordinary fair wakeup preemption, but
                        // the Idle case above still enforces fair class order.
                        false
                    } else if woken_entity
                        .fair()
                        .is_none_or(|fair| !fair.is_eligible(fair_virtual_time))
                    {
                        false
                    } else {
                        fair_deadline(woken_entity).saturating_add(wakeup_granularity_ns)
                            < fair_deadline(self.entity)
                    }
                }
            },
        }
    }
}

fn deadline_key(entity: SchedulingEntity) -> u64 {
    entity
        .deadline()
        .map_or(u64::MAX, |deadline| deadline.absolute_deadline_ns())
}

fn fair_deadline(entity: SchedulingEntity) -> u64 {
    entity
        .fair()
        .map_or(u64::MAX, |fair| fair.virtual_deadline())
}

/// Result of one allocation-free local dispatch charge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DispatchCharge {
    pub(crate) slice_expired: bool,
    pub(crate) deadline_overrun: bool,
}

fn nonzero_deadline(deadline_ns: u64) -> Option<u64> {
    (deadline_ns != 0).then_some(deadline_ns)
}

fn earliest(current: Option<u64>, candidate: u64) -> Option<u64> {
    Some(current.map_or(candidate, |current| current.min(candidate)))
}

/// Stable, allocation-free scheduler state used by deterministic model tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuSnapshot {
    owner: CpuId,
    current: Option<ThreadId>,
    runnable: usize,
    need_resched: bool,
}

impl CpuSnapshot {
    pub(crate) fn capture(cpu: &CpuLocal) -> Self {
        Self {
            owner: cpu.owner,
            current: cpu.current,
            runnable: cpu.runnable_count(),
            need_resched: cpu.needs_reschedule(),
        }
    }

    /// Returns the owner CPU.
    pub const fn owner(self) -> CpuId {
        self.owner
    }

    /// Returns the current thread.
    pub const fn current(self) -> Option<ThreadId> {
        self.current
    }

    /// Returns the number of runnable threads.
    pub const fn runnable(self) -> usize {
        self.runnable
    }

    /// Returns the sticky preemption state.
    pub const fn need_resched(self) -> bool {
        self.need_resched
    }
}

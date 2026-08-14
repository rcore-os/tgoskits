//! Task-embedded runqueue linkage and immutable rq snapshots.

use alloc::{boxed::Box, sync::Arc};
use core::{cell::UnsafeCell, fmt};

use super::{deadline, deadline_pushable, realtime};
use crate::{
    ActiveSchedulingState, CpuSet, SchedulePolicy, SchedulingEntity, ThreadCore, ThreadId,
};

/// Task-control facts published into one owner rq transaction.
#[derive(Clone, Debug)]
pub(crate) struct RqTaskMetadata {
    pub(crate) affinity: Arc<CpuSet>,
    pub(crate) deadline_bandwidth_scaled: u64,
    pub(crate) runtime_binding: crate::runtime::ThreadRuntimeBinding,
}

#[cfg(test)]
impl RqTaskMetadata {
    pub(crate) fn test(cpu_count: usize) -> Self {
        Self {
            affinity: Arc::new(CpuSet::all(cpu_count)),
            deadline_bandwidth_scaled: 0,
            runtime_binding: crate::runtime::ThreadRuntimeBinding::new(
                crate::runtime::ExecutionContextHandle::NONE,
                crate::runtime::AddressSpaceHandle::NONE,
            ),
        }
    }
}

/// Scheduling-class linkage prepared with each thread, like Linux embedding
/// `sched_entity`, `sched_rt_entity`, and `sched_dl_entity` in `task_struct`.
pub(crate) struct RunQueueNodeStorage {
    deadline: UnsafeCell<Option<Box<deadline::DeadlineNode>>>,
    deadline_pushable: UnsafeCell<Option<Box<deadline_pushable::DeadlinePushableNode>>>,
    fair: UnsafeCell<Option<Box<crate::scheduler::fair_queue::FairNode>>>,
    realtime: UnsafeCell<Option<Box<realtime::RealtimeNode>>>,
    realtime_pushable: UnsafeCell<Option<Box<realtime::RealtimePushableNode>>>,
}

impl RunQueueNodeStorage {
    pub(crate) fn new() -> Self {
        Self {
            deadline: UnsafeCell::new(Some(deadline::DeadlineNode::empty())),
            deadline_pushable: UnsafeCell::new(Some(
                deadline_pushable::DeadlinePushableNode::empty(),
            )),
            fair: UnsafeCell::new(Some(crate::scheduler::fair_queue::FairNode::empty())),
            realtime: UnsafeCell::new(Some(realtime::RealtimeNode::empty())),
            realtime_pushable: UnsafeCell::new(Some(realtime::RealtimePushableNode::empty())),
        }
    }

    pub(crate) unsafe fn take_deadline(&self) -> Box<deadline::DeadlineNode> {
        unsafe { &mut *self.deadline.get() }
            .take()
            .expect("one thread cannot own two Deadline runqueue links")
    }

    pub(crate) unsafe fn return_deadline(&self, node: Box<deadline::DeadlineNode>) {
        assert!(
            unsafe { &mut *self.deadline.get() }.replace(node).is_none(),
            "unlinked Deadline node must have one storage owner"
        );
    }

    pub(crate) unsafe fn take_deadline_pushable(
        &self,
    ) -> Box<deadline_pushable::DeadlinePushableNode> {
        unsafe { &mut *self.deadline_pushable.get() }
            .take()
            .expect("one thread cannot own two Deadline pushable links")
    }

    pub(crate) unsafe fn return_deadline_pushable(
        &self,
        node: Box<deadline_pushable::DeadlinePushableNode>,
    ) {
        assert!(
            unsafe { &mut *self.deadline_pushable.get() }
                .replace(node)
                .is_none(),
            "unlinked Deadline pushable node must have one storage owner"
        );
    }

    pub(crate) unsafe fn take_fair(&self) -> Box<crate::scheduler::fair_queue::FairNode> {
        unsafe { &mut *self.fair.get() }
            .take()
            .expect("one thread cannot own two fair runqueue links")
    }

    pub(crate) unsafe fn return_fair(&self, node: Box<crate::scheduler::fair_queue::FairNode>) {
        assert!(
            unsafe { &mut *self.fair.get() }.replace(node).is_none(),
            "unlinked fair node must have one storage owner"
        );
    }

    pub(crate) unsafe fn take_realtime(&self) -> Box<realtime::RealtimeNode> {
        unsafe { &mut *self.realtime.get() }
            .take()
            .expect("one thread cannot own two RT runqueue links")
    }

    pub(crate) unsafe fn return_realtime(&self, node: Box<realtime::RealtimeNode>) {
        assert!(
            unsafe { &mut *self.realtime.get() }.replace(node).is_none(),
            "unlinked RT node must have one storage owner"
        );
    }

    pub(crate) unsafe fn take_realtime_pushable(&self) -> Box<realtime::RealtimePushableNode> {
        unsafe { &mut *self.realtime_pushable.get() }
            .take()
            .expect("one thread cannot own two RT pushable links")
    }

    pub(crate) unsafe fn return_realtime_pushable(
        &self,
        node: Box<realtime::RealtimePushableNode>,
    ) {
        assert!(
            unsafe { &mut *self.realtime_pushable.get() }
                .replace(node)
                .is_none(),
            "unlinked RT pushable node must have one storage owner"
        );
    }
}

// SAFETY: individual node transfers are serialized by task placement and the
// owning CpuRunQueueState IRQ lock. No unrelated handle accesses a node slot.
unsafe impl Sync for RunQueueNodeStorage {}

impl fmt::Debug for RunQueueNodeStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunQueueNodeStorage")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct QueuedThread {
    pub(crate) id: ThreadId,
    pub(crate) active: ActiveSchedulingState,
    pub(crate) core: Arc<ThreadCore>,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) metadata: RqTaskMetadata,
    /// The affinity snapshot allows this queued entity to leave its owner rq.
    pub(crate) migration_capable: bool,
    pub(crate) balance_scan_epoch: u64,
    pub(crate) sequence: u64,
}

/// Read-only observation of one rq-owned entity.
#[derive(Clone, Debug)]
pub(crate) struct QueuedThreadSnapshot {
    pub(crate) id: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) base_entity: SchedulingEntity,
    pub(crate) core: Arc<ThreadCore>,
    pub(crate) rt_quota_exempt: bool,
    pub(crate) metadata: RqTaskMetadata,
}

impl From<&QueuedThread> for QueuedThreadSnapshot {
    fn from(thread: &QueuedThread) -> Self {
        Self {
            id: thread.id,
            policy: thread.active.policy(),
            entity: thread.active.entity().clone(),
            base_entity: thread.active.base_entity().clone(),
            core: Arc::clone(&thread.core),
            rt_quota_exempt: thread.rt_quota_exempt,
            metadata: thread.metadata.clone(),
        }
    }
}

impl QueuedThreadSnapshot {
    pub(crate) fn policy(&self) -> SchedulePolicy {
        self.policy
    }

    pub(crate) const fn placement_demand(&self) -> u64 {
        self.policy.placement_demand()
    }
}

/// Result of one class pick under the owner rq lock.
#[derive(Debug)]
pub(crate) enum PickedThread {
    Owned(QueuedThread),
    Linked(QueuedThreadSnapshot),
}

impl PickedThread {
    pub(crate) const fn id(&self) -> ThreadId {
        match self {
            Self::Owned(thread) => thread.id,
            Self::Linked(thread) => thread.id,
        }
    }

    pub(crate) fn core(&self) -> &Arc<ThreadCore> {
        match self {
            Self::Owned(thread) => &thread.core,
            Self::Linked(thread) => &thread.core,
        }
    }

    pub(crate) fn policy(&self) -> SchedulePolicy {
        match self {
            Self::Owned(thread) => thread.active.policy(),
            Self::Linked(thread) => thread.policy,
        }
    }

    pub(crate) fn metadata(&self) -> &RqTaskMetadata {
        match self {
            Self::Owned(thread) => &thread.metadata,
            Self::Linked(thread) => &thread.metadata,
        }
    }

    pub(crate) const fn rt_quota_exempt(&self) -> bool {
        match self {
            Self::Owned(thread) => thread.rt_quota_exempt,
            Self::Linked(thread) => thread.rt_quota_exempt,
        }
    }
}

impl QueuedThread {
    pub(crate) fn new(
        id: ThreadId,
        active: ActiveSchedulingState,
        core: Arc<ThreadCore>,
        rt_quota_exempt: bool,
        migration_capable: bool,
        metadata: RqTaskMetadata,
    ) -> Self {
        Self {
            id,
            active,
            core,
            rt_quota_exempt,
            metadata,
            migration_capable,
            balance_scan_epoch: 0,
            sequence: 0,
        }
    }

    pub(crate) fn policy(&self) -> SchedulePolicy {
        self.active.policy()
    }

    pub(crate) fn entity(&self) -> &SchedulingEntity {
        self.active.entity()
    }

    pub(crate) fn entity_snapshot(&self) -> SchedulingEntity {
        self.entity().clone()
    }

    pub(crate) fn into_active(self) -> ActiveSchedulingState {
        self.active
    }
}

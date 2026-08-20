use super::*;
use crate::inbox::InboxOperation;

#[derive(Debug)]
pub(super) struct RemoteDeliveryState {
    owner_control_inbox: SchedulerInbox,
    balance_request_node: InboxNode,
}

/// A fully reserved queued-task migration publication.
///
/// The target CPU hotplug lease and the task's intrusive inbox reservation are
/// acquired before the source runqueue changes `on_rq` or `task_cpu`. Once this
/// value exists, migration publication is an invariant-only commit, matching
/// Linux's rq-locked `deactivate_task -> set_task_cpu -> activate_task` path.
#[derive(Debug)]
pub(crate) struct PreparedMigrationDelivery {
    publication: Option<OwnedCpuRemotePublication>,
    core: Option<Arc<ThreadCore>>,
    source: CpuId,
    target: CpuId,
    placement_demand: u64,
}

impl PreparedMigrationDelivery {
    pub(crate) fn prepare(
        target_remote: &Arc<CpuRemote>,
        core: &Arc<ThreadCore>,
        source: CpuId,
        target: CpuId,
    ) -> Result<Self, TaskError> {
        let publication = target_remote
            .begin_owned_publication()
            .ok_or(TaskError::CpuOffline(target.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Err(TaskError::NotReady);
        }
        Ok(Self {
            publication: Some(publication),
            core: Some(Arc::clone(core)),
            source,
            target,
            placement_demand: core.effective_placement_demand(),
        })
    }

    pub(crate) const fn target(&self) -> CpuId {
        self.target
    }

    pub(crate) fn commit(mut self) {
        let publication = self
            .publication
            .take()
            .expect("prepared migration must retain its target CPU lease");
        let core = self
            .core
            .take()
            .expect("prepared migration must retain its thread delivery lease");
        let thread = core.id();
        let pointer = Arc::into_raw(core);
        let node = unsafe {
            // SAFETY: the transferred Arc count keeps the embedded node pinned
            // until one target-owner drain reconstructs and releases it.
            Pin::new_unchecked((*pointer).migration_node())
        };
        let message = InboxMessage::migration_with_payload(
            thread,
            self.source,
            self.target,
            thread.generation() as u64,
            self.placement_demand,
            pointer.expose_provenance(),
        );
        match publication.publish_owner_control(node, message) {
            PublishResult::Published => {}
            PublishResult::AlreadyPending => unsafe {
                // SAFETY: a coalesced publication did not consume this Arc.
                // The older carrier observes the generation-bearing state.
                let retained = Arc::from_raw(pointer);
                retained.cancel_scheduler_inbox_delivery();
                drop(retained);
            },
            PublishResult::WrongKind => unsafe {
                // SAFETY: a prepared target lease and typed migration node make
                // rejection an internal invariant, not a recoverable fallback.
                let retained = Arc::from_raw(pointer);
                retained.cancel_scheduler_inbox_delivery();
                drop(retained);
                task_runtime::fatal_invariant(0x4d49_4701, thread.as_u64() as usize);
            },
        }
    }
}

impl Drop for PreparedMigrationDelivery {
    fn drop(&mut self) {
        if let Some(core) = self.core.take() {
            core.cancel_scheduler_inbox_delivery();
        }
    }
}

impl RemoteDeliveryState {
    pub(super) const fn new() -> Self {
        Self {
            owner_control_inbox: SchedulerInbox::new(InboxKind::OwnerControl),
            balance_request_node: InboxNode::new(InboxKind::OwnerControl),
        }
    }
}

impl CpuRemote {
    pub(crate) fn publish_owner_control(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let Some(remote_publication) = self.begin_publication() else {
            return PublishResult::WrongKind;
        };
        remote_publication.publish_owner_control(node, message)
    }

    pub(super) fn publish_owner_control_owned(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let _irq = IrqScope::enter();
        let _idle_pull_work = self.begin_idle_pull_work();
        let migration = message.operation() == InboxOperation::Migration;
        if migration {
            self.reserve_incoming_migration(message.placement_demand());
        }
        let (result, head_became_non_empty) = self
            .delivery
            .owner_control_inbox
            .publish_with_head_transition(node, message);
        if migration && result != PublishResult::Published {
            self.release_incoming_migration_demand(message.placement_demand());
        }
        if matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ) {
            let publication = self.request_scheduler_work_owned();
            if head_became_non_empty {
                self.deliver_scheduler_work_owned(publication);
            }
        }
        result
    }

    pub(crate) fn balance_request_node(&self) -> Pin<&'static InboxNode> {
        let node = &self.delivery.balance_request_node as *const InboxNode;
        // SAFETY: TaskSystem owns this Arc-backed endpoint until shutdown. The
        // embedded node is never moved and coalesces publications for one CPU.
        unsafe { Pin::new_unchecked(&*node) }
    }

    pub(crate) fn owner_control_inbox(&self) -> &SchedulerInbox {
        &self.delivery.owner_control_inbox
    }

    pub(crate) fn has_remote_work(&self) -> bool {
        self.delivery.owner_control_inbox.has_pending()
    }
}

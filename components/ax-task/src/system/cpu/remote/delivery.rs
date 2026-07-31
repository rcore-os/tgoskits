use super::*;
use crate::inbox::InboxOperation;

#[derive(Debug)]
pub(super) struct RemoteDeliveryState {
    wake_inbox: SchedulerInbox,
    owner_control_inbox: SchedulerInbox,
    balance_request_node: InboxNode,
}

impl RemoteDeliveryState {
    pub(super) const fn new() -> Self {
        Self {
            wake_inbox: SchedulerInbox::new(InboxKind::RemoteWake),
            owner_control_inbox: SchedulerInbox::new(InboxKind::OwnerControl),
            balance_request_node: InboxNode::new(InboxKind::OwnerControl),
        }
    }
}

impl CpuRemote {
    #[cfg(test)]
    pub(crate) fn publish_remote_wake(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let Some(carrier) = self.begin_wake_carrier() else {
            return PublishResult::WrongKind;
        };
        carrier.publish_remote_wake(node, message)
    }

    pub(super) fn publish_remote_wake_owned(
        &self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        let _irq = IrqScope::enter();
        let _idle_pull_work = self.begin_idle_pull_work();
        let (result, head_became_non_empty) = self
            .delivery
            .wake_inbox
            .publish_with_head_transition(node, message);
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
            self.reserve_incoming_migration();
        }
        let (result, head_became_non_empty) = self
            .delivery
            .owner_control_inbox
            .publish_with_head_transition(node, message);
        if migration && result != PublishResult::Published {
            self.complete_incoming_migrations(1);
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

    pub(crate) fn remote_wake_inbox(&self) -> &SchedulerInbox {
        &self.delivery.wake_inbox
    }

    pub(crate) fn owner_control_inbox(&self) -> &SchedulerInbox {
        &self.delivery.owner_control_inbox
    }

    pub(crate) fn has_remote_work(&self) -> bool {
        self.delivery.wake_inbox.has_pending() || self.delivery.owner_control_inbox.has_pending()
    }
}

use core::pin::Pin;

use ax_task::{
    CpuId, ThreadId,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    timer::{
        ExpiredTaskDeadline, TaskDeadlineExpireRequest, TaskDeadlineKind, TaskDeadlineNode,
        TaskDeadlineQueue,
    },
};

#[test]
fn timer_irq_work_is_bounded() {
    let timers = [timer(0), timer(1), timer(2)];
    let mut queue = TaskDeadlineQueue::new(3);
    let _registrations = timers
        .iter()
        .enumerate()
        .map(|(generation, node)| {
            queue
                .arm(
                    node.as_ref(),
                    10,
                    TaskDeadlineKind::park_timeout(generation as u64 + 1),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut output = [ExpiredTaskDeadline::EMPTY; 3];

    let batch = queue.expire(TaskDeadlineExpireRequest::new(10, 2, 1), &mut output);

    assert_eq!(batch.processed(), 2);
    assert_eq!(batch.expired(), 2);
    assert!(batch.pending());
}

#[test]
fn owner_control_publication_coalesces_and_drain_is_bounded() {
    let inbox = SchedulerInbox::new(InboxKind::OwnerControl);
    let first = inbox_node(InboxKind::OwnerControl);
    let second = inbox_node(InboxKind::OwnerControl);
    let first_message = InboxMessage::migration(thread(1), CpuId::new(0), CpuId::new(1), 1);
    let second_message = InboxMessage::migration(thread(2), CpuId::new(0), CpuId::new(1), 2);
    assert_eq!(
        inbox.publish(first.pin(), first_message),
        PublishResult::Published
    );
    assert_eq!(
        inbox.publish(first.pin(), first_message),
        PublishResult::AlreadyPending
    );
    assert_eq!(
        inbox.publish(second.pin(), second_message),
        PublishResult::Published
    );
    let mut output = [InboxMessage::EMPTY; 2];

    let batch = inbox.drain(1, &mut output);

    assert_eq!(batch.drained(), 1);
    assert!(batch.pending());
    assert_eq!(output[0].thread_id(), thread(1));
}

fn timer(slot: u32) -> Box<TaskDeadlineNode> {
    Box::new(TaskDeadlineNode::for_thread(thread(slot)))
}

struct TestInboxNode(Pin<Box<InboxNode>>);

impl TestInboxNode {
    fn pin(&self) -> Pin<&'static InboxNode> {
        let node = self.0.as_ref().get_ref() as *const InboxNode;
        unsafe {
            // The test drains every published node before dropping the fixture.
            Pin::new_unchecked(&*node)
        }
    }
}

fn inbox_node(kind: InboxKind) -> TestInboxNode {
    TestInboxNode(Box::pin(InboxNode::new(kind)))
}

fn thread(slot: u32) -> ThreadId {
    ThreadId::from_parts(slot, 1)
}

#[unsafe(no_mangle)]
extern "Rust" fn __ax_task_0_7_fatal_invariant(code: u32, argument: usize) -> ! {
    panic!("bounded IRQ fixture hit ax-task invariant {code:#x} ({argument:#x})")
}

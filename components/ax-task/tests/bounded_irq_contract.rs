use ax_task::{
    ThreadId,
    runtime::{MonotonicDeadline, MonotonicInstant},
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
                    MonotonicDeadline::from_nanos(10).unwrap(),
                    TaskDeadlineKind::park_timeout(generation as u64 + 1),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut output = [ExpiredTaskDeadline::EMPTY; 3];

    let batch = queue.expire(
        TaskDeadlineExpireRequest::new(MonotonicInstant::from_nanos(10).unwrap(), 2),
        &mut output,
    );

    assert_eq!(batch.processed(), 2);
    assert_eq!(batch.expired(), 2);
    assert!(batch.pending());
}

fn timer(slot: u32) -> Box<TaskDeadlineNode> {
    Box::new(TaskDeadlineNode::for_thread(thread(slot)))
}

fn thread(slot: u32) -> ThreadId {
    ThreadId::from_parts(slot, 1)
}

#[unsafe(no_mangle)]
extern "Rust" fn __ax_task_0_7_fatal_invariant(code: u32, argument: usize) -> ! {
    panic!("bounded IRQ fixture hit ax-task invariant {code:#x} ({argument:#x})")
}

use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_runtime::workqueue::{
    QueueWorkResult, WORK_BATCH_LIMIT, WorkItem, WorkOutcome, WorkPriority, WorkQueue,
    WorkQueueError, WorkQueueState, WorkQueueSystem,
};

fn pinned(work: &'static WorkItem) -> Pin<&'static WorkItem> {
    // SAFETY: every test allocation is leaked, so the intrusive node has a
    // stable address for the entire queue-system lifetime.
    unsafe { Pin::new_unchecked(work) }
}

fn count_callback(data: usize) -> WorkOutcome {
    // SAFETY: tests pass only leaked `AtomicUsize` pointers as callback data.
    let counter = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
    counter.fetch_add(1, Ordering::Relaxed);
    WorkOutcome::Complete
}

fn counting_work(counter: &'static AtomicUsize) -> Pin<&'static WorkItem> {
    pinned(Box::leak(Box::new(WorkItem::new(
        count_callback,
        ptr::from_ref(counter).expose_provenance(),
    ))))
}

fn queue_system<const CPU_COUNT: usize>() -> &'static WorkQueueSystem<CPU_COUNT> {
    Box::leak(Box::new(WorkQueueSystem::new()))
}

#[test]
fn repeated_queue_coalesces_one_pending_execution() {
    let queue = queue_system::<1>();
    let counter = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(counter);

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::AlreadyPending
    );

    let batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(batch.executed(), 1);
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    assert!(!batch.pending());
}

struct RunningGate {
    entered: AtomicBool,
    release: AtomicBool,
    calls: AtomicUsize,
    callback_requeue: bool,
}

fn gated_callback(data: usize) -> WorkOutcome {
    // SAFETY: the test leaks this synchronization gate before publishing work.
    let gate = unsafe { &*ptr::with_exposed_provenance::<RunningGate>(data) };
    if gate.calls.fetch_add(1, Ordering::Relaxed) != 0 {
        return WorkOutcome::Complete;
    }
    gate.entered.store(true, Ordering::Release);
    // Test-only gate: it holds the callback in RUNNING so another host thread
    // can deterministically model an IRQ producer. Production callbacks retain
    // the module's bounded/non-blocking contract.
    while !gate.release.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    if gate.callback_requeue {
        WorkOutcome::Requeue
    } else {
        WorkOutcome::Complete
    }
}

#[test]
fn queue_while_running_requests_exactly_one_rerun() {
    let queue = queue_system::<1>();
    let gate: &'static RunningGate = Box::leak(Box::new(RunningGate {
        entered: AtomicBool::new(false),
        release: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        callback_requeue: false,
    }));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        gated_callback,
        ptr::from_ref(gate).expose_provenance(),
    ))));

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    let worker = std::thread::spawn(move || queue.service_batch(0, WorkPriority::Normal).unwrap());
    while !gate.entered.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::RerunRequested
    );
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::AlreadyPending
    );
    gate.release.store(true, Ordering::Release);
    let batch = worker.join().unwrap();

    assert_eq!(gate.calls.load(Ordering::Relaxed), 2);
    assert_eq!(batch.executed(), 2);
    assert!(!batch.pending());
}

#[test]
fn running_work_keeps_its_owner_route_when_requeued_from_another_route() {
    let queue = queue_system::<2>();
    let gate: &'static RunningGate = Box::leak(Box::new(RunningGate {
        entered: AtomicBool::new(false),
        release: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        callback_requeue: false,
    }));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        gated_callback,
        ptr::from_ref(gate).expose_provenance(),
    ))));

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    let owner = std::thread::spawn(move || queue.service_batch(0, WorkPriority::Normal).unwrap());
    while !gate.entered.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    assert_eq!(
        queue.queue_work_on(1, WorkPriority::High, work).unwrap(),
        QueueWorkResult::RerunRequested
    );
    gate.release.store(true, Ordering::Release);
    let owner_batch = owner.join().unwrap();
    let foreign_batch = queue.service_batch(1, WorkPriority::High).unwrap();

    assert_eq!(owner_batch.executed(), 2);
    assert_eq!(foreign_batch.executed(), 0);
    assert_eq!(gate.calls.load(Ordering::Relaxed), 2);
    assert!(!owner_batch.pending());
    assert!(!foreign_batch.pending());
}

#[test]
fn queued_work_keeps_its_owner_route_when_requeued_from_another_route() {
    let queue = queue_system::<2>();
    let counter = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(counter);

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        queue.queue_work_on(1, WorkPriority::High, work).unwrap(),
        QueueWorkResult::AlreadyPending
    );

    let foreign_batch = queue.service_batch(1, WorkPriority::High).unwrap();
    let owner_batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(foreign_batch.executed(), 0);
    assert_eq!(owner_batch.executed(), 1);
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

fn outcome_requeue_once(data: usize) -> WorkOutcome {
    // SAFETY: this test passes one leaked counter as callback data.
    let calls = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
    if calls.fetch_add(1, Ordering::Relaxed) == 0 {
        WorkOutcome::Requeue
    } else {
        WorkOutcome::Complete
    }
}

fn outcome_always_requeue(data: usize) -> WorkOutcome {
    // SAFETY: this test passes one leaked counter as callback data.
    let calls = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
    calls.fetch_add(1, Ordering::Relaxed);
    WorkOutcome::Requeue
}

#[test]
fn callback_outcome_requeues_without_recursive_submission() {
    let queue = queue_system::<1>();
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        outcome_requeue_once,
        ptr::from_ref(calls).expose_provenance(),
    ))));
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );

    let batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(batch.executed(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(!batch.pending());
}

#[test]
fn one_self_requeueing_item_cannot_exceed_the_worker_batch() {
    let queue = queue_system::<1>();
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        outcome_always_requeue,
        ptr::from_ref(calls).expose_provenance(),
    ))));
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );

    let batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(batch.executed(), WORK_BATCH_LIMIT);
    assert!(batch.saturated());
    assert!(batch.pending());
    assert_eq!(calls.load(Ordering::Relaxed), WORK_BATCH_LIMIT);

    let cancellation = queue.begin_cancel(work);
    let cancelled = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(cancelled.cancelled(), 1);
    assert!(cancellation.is_complete());
}

#[test]
fn callback_requeue_and_running_time_queue_merge_into_one_rerun() {
    let queue = queue_system::<1>();
    let gate: &'static RunningGate = Box::leak(Box::new(RunningGate {
        entered: AtomicBool::new(false),
        release: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        callback_requeue: true,
    }));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        gated_callback,
        ptr::from_ref(gate).expose_provenance(),
    ))));
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );

    let worker = std::thread::spawn(move || queue.service_batch(0, WorkPriority::Normal).unwrap());
    while !gate.entered.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::RerunRequested
    );
    gate.release.store(true, Ordering::Release);
    let batch = worker.join().unwrap();

    assert_eq!(gate.calls.load(Ordering::Relaxed), 2);
    assert_eq!(batch.executed(), 2);
}

#[test]
fn cancellation_during_callback_suppresses_its_requeue_outcome() {
    let queue = queue_system::<1>();
    let gate: &'static RunningGate = Box::leak(Box::new(RunningGate {
        entered: AtomicBool::new(false),
        release: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        callback_requeue: true,
    }));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        gated_callback,
        ptr::from_ref(gate).expose_provenance(),
    ))));
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );

    let worker = std::thread::spawn(move || queue.service_batch(0, WorkPriority::Normal).unwrap());
    while !gate.entered.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let cancellation = queue.begin_cancel(work);
    assert!(cancellation.was_pending());
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::CancelInProgress
    );
    gate.release.store(true, Ordering::Release);
    let batch = worker.join().unwrap();

    assert_eq!(batch.executed(), 1);
    assert!(!batch.pending());
    assert_eq!(gate.calls.load(Ordering::Relaxed), 1);
    assert!(cancellation.is_complete());
    assert!(work.is_idle());
}

#[test]
fn worker_batch_is_bounded_at_sixty_four_callbacks() {
    let queue = queue_system::<1>();
    let counter = Box::leak(Box::new(AtomicUsize::new(0)));
    for _ in 0..WORK_BATCH_LIMIT + 1 {
        assert_eq!(
            queue
                .queue_work_on(0, WorkPriority::Normal, counting_work(counter))
                .unwrap(),
            QueueWorkResult::Queued
        );
    }

    let first = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(first.executed(), WORK_BATCH_LIMIT);
    assert!(first.saturated());
    assert!(first.pending());

    let second = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(second.executed(), 1);
    assert!(!second.pending());
    assert_eq!(counter.load(Ordering::Relaxed), WORK_BATCH_LIMIT + 1);
}

#[test]
fn worker_rejects_a_batch_above_sixty_four_nodes() {
    let queue = queue_system::<1>();

    assert_eq!(
        queue.service_batch_with_limit(0, WorkPriority::Normal, WORK_BATCH_LIMIT + 1,),
        Err(WorkQueueError::BatchLimitExceeded {
            requested: WORK_BATCH_LIMIT + 1,
            maximum: WORK_BATCH_LIMIT,
        })
    );
}

#[test]
fn normal_and_high_priority_workers_are_independent_domains() {
    let queue = queue_system::<1>();
    let normal = Box::leak(Box::new(AtomicUsize::new(0)));
    let high = Box::leak(Box::new(AtomicUsize::new(0)));
    assert_eq!(
        queue
            .queue_work_on(0, WorkPriority::Normal, counting_work(normal))
            .unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        queue
            .queue_work_on(0, WorkPriority::High, counting_work(high))
            .unwrap(),
        QueueWorkResult::Queued
    );

    assert!(queue.has_pending(0, WorkPriority::Normal).unwrap());
    assert_eq!(
        queue
            .service_batch(0, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );
    assert_eq!(normal.load(Ordering::Relaxed), 1);
    assert_eq!(high.load(Ordering::Relaxed), 0);
    assert!(!queue.has_pending(0, WorkPriority::Normal).unwrap());
    assert!(queue.has_pending(0, WorkPriority::High).unwrap());

    assert_eq!(
        queue
            .service_batch(0, WorkPriority::High)
            .unwrap()
            .executed(),
        1
    );
    assert_eq!(high.load(Ordering::Relaxed), 1);
}

#[test]
fn per_cpu_worker_lanes_do_not_steal_each_others_work() {
    let queue = queue_system::<2>();
    let cpu_one = Box::leak(Box::new(AtomicUsize::new(0)));
    assert_eq!(
        queue
            .queue_work_on(1, WorkPriority::Normal, counting_work(cpu_one))
            .unwrap(),
        QueueWorkResult::Queued
    );

    let cpu_zero_batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(cpu_zero_batch.executed(), 0);
    assert_eq!(cpu_zero_batch.cancelled(), 0);
    assert!(!cpu_zero_batch.pending());
    assert_eq!(cpu_one.load(Ordering::Relaxed), 0);
    assert!(queue.has_pending(1, WorkPriority::Normal).unwrap());

    assert_eq!(
        queue
            .service_batch(1, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );
    assert_eq!(cpu_one.load(Ordering::Relaxed), 1);
}

static IRQ_MODEL_QUEUE: WorkQueueSystem<1> = WorkQueueSystem::new();
static IRQ_MODEL_CALLS: AtomicUsize = AtomicUsize::new(0);

fn irq_model_callback(_data: usize) -> WorkOutcome {
    IRQ_MODEL_CALLS.fetch_add(1, Ordering::Relaxed);
    WorkOutcome::Complete
}

static IRQ_MODEL_WORK: WorkItem = WorkItem::new(irq_model_callback, 0);

fn hard_irq_submit_model() -> QueueWorkResult {
    IRQ_MODEL_QUEUE
        .queue_work_on(0, WorkPriority::High, pinned(&IRQ_MODEL_WORK))
        .unwrap()
}

#[test]
fn queue_work_on_has_a_preallocated_hard_irq_submission_model() {
    IRQ_MODEL_CALLS.store(0, Ordering::Relaxed);
    assert_eq!(hard_irq_submit_model(), QueueWorkResult::Queued);
    assert_eq!(hard_irq_submit_model(), QueueWorkResult::AlreadyPending);
    assert_eq!(IRQ_MODEL_CALLS.load(Ordering::Relaxed), 0);

    assert_eq!(
        IRQ_MODEL_QUEUE
            .service_batch(0, WorkPriority::High)
            .unwrap()
            .executed(),
        1
    );
    assert_eq!(IRQ_MODEL_CALLS.load(Ordering::Relaxed), 1);
}

#[test]
fn cancel_tombstones_queued_work_until_the_worker_consumes_it() {
    let queue = queue_system::<1>();
    let counter = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(counter);
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );

    let cancellation = queue.begin_cancel(work);
    assert!(cancellation.was_pending());
    assert!(!cancellation.is_complete());
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::CancelInProgress
    );

    let batch = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(batch.executed(), 0);
    assert_eq!(batch.cancelled(), 1);
    assert!(cancellation.is_complete());
    assert_eq!(counter.load(Ordering::Relaxed), 0);

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        queue
            .service_batch(0, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );
}

#[test]
fn completion_tokens_are_bound_to_their_original_work_item() {
    let queue = queue_system::<1>();
    let first_calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let second_calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let first = counting_work(first_calls);
    let second = counting_work(second_calls);
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, first).unwrap(),
        QueueWorkResult::Queued
    );
    let first_flush = queue.begin_flush(first);
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::High, second).unwrap(),
        QueueWorkResult::Queued
    );

    assert_eq!(
        queue
            .service_batch(0, WorkPriority::High)
            .unwrap()
            .executed(),
        1
    );
    // There is no WorkItem argument to `is_complete`: a ticket cannot be
    // accidentally checked against `second` or any other node.
    assert!(!first_flush.is_complete());
    assert_eq!(
        queue
            .service_batch(0, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );
    assert!(first_flush.is_complete());

    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, first).unwrap(),
        QueueWorkResult::Queued
    );
    let first_cancel = queue.begin_cancel(first);
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::High, second).unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        queue
            .service_batch(0, WorkPriority::High)
            .unwrap()
            .executed(),
        1
    );
    assert!(!first_cancel.is_complete());
    let cancelled = queue.service_batch(0, WorkPriority::Normal).unwrap();
    assert_eq!(cancelled.executed(), 0);
    assert_eq!(cancelled.cancelled(), 1);
    assert!(first_cancel.is_complete());
}

#[test]
fn one_intrusive_item_cannot_cross_independent_queue_systems() {
    let first_queue = queue_system::<1>();
    let second_queue = queue_system::<1>();
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(calls);
    assert_eq!(
        first_queue
            .queue_work_on(0, WorkPriority::Normal, work)
            .unwrap(),
        QueueWorkResult::Queued
    );
    assert_eq!(
        first_queue
            .service_batch(0, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );

    assert_eq!(
        second_queue.queue_work_on(0, WorkPriority::Normal, work),
        Err(WorkQueueError::ForeignSystem)
    );
}

#[test]
fn logical_domain_owns_policy_and_drain_state_but_no_worker() {
    let queue = Box::leak(Box::new(WorkQueue::new(3, WorkPriority::High)));
    // SAFETY: the leaked domain has a stable shutdown-lifetime address.
    let queue = unsafe { Pin::new_unchecked(&*queue) };
    assert_eq!(queue.cpu(), 3);
    assert_eq!(queue.priority(), WorkPriority::High);
    assert_eq!(queue.state(), WorkQueueState::Accepting);

    let drain = queue.begin_drain().unwrap();
    assert!(drain.is_complete());
    assert_eq!(queue.state(), WorkQueueState::Drained);
    assert!(matches!(
        queue.begin_drain(),
        Err(WorkQueueError::DomainNotAccepting)
    ));
}

#[cfg(feature = "workqueue")]
#[test]
fn never_submitted_runtime_work_has_noop_flush_and_cancel() {
    let queue = Box::leak(Box::new(WorkQueue::new(0, WorkPriority::Normal)));
    let queue = unsafe {
        // SAFETY: the fixture is leaked for the runtime facade's static pin contract.
        Pin::new_unchecked(&*queue)
    };
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(calls);

    assert_eq!(queue.flush_work(work), Ok(()));
    assert_eq!(queue.cancel_work_sync(work), Ok(()));
    assert!(work.is_idle());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[cfg(feature = "workqueue")]
#[test]
fn runtime_domain_rejects_submission_before_its_fixed_worker_is_published() {
    let queue = Box::leak(Box::new(WorkQueue::new(0, WorkPriority::Normal)));
    // SAFETY: both the logical domain and WorkItem below are leaked for the
    // process lifetime, satisfying the intrusive runtime facade contract.
    let queue = unsafe { Pin::new_unchecked(&*queue) };
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = counting_work(calls);

    assert_eq!(
        queue.queue_work_on(work),
        Err(WorkQueueError::WorkerNotInitialized)
    );
    assert!(work.is_idle());
    assert_eq!(queue.state(), WorkQueueState::Accepting);
}

#[test]
fn flush_ticket_waits_only_for_work_accepted_before_its_snapshot() {
    let queue = queue_system::<1>();
    let calls = Box::leak(Box::new(AtomicUsize::new(0)));
    let work = pinned(Box::leak(Box::new(WorkItem::new(
        outcome_requeue_once,
        ptr::from_ref(calls).expose_provenance(),
    ))));
    assert_eq!(
        queue.queue_work_on(0, WorkPriority::Normal, work).unwrap(),
        QueueWorkResult::Queued
    );
    let first_flush = queue.begin_flush(work);

    let first = queue
        .service_batch_with_limit(0, WorkPriority::Normal, 1)
        .unwrap();
    assert_eq!(first.executed(), 1);
    assert!(first.pending());
    assert!(first_flush.is_complete());

    let rerun_flush = queue.begin_flush(work);
    assert!(!rerun_flush.is_complete());
    assert_eq!(
        queue
            .service_batch(0, WorkPriority::Normal)
            .unwrap()
            .executed(),
        1
    );
    assert!(rerun_flush.is_complete());
}

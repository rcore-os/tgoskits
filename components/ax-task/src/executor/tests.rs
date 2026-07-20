use alloc::{boxed::Box, rc::Rc, sync::Arc};
use core::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use super::*;
use crate::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

#[test]
fn coalesces_repeated_wakes_into_one_poll() {
    let fixture = executor();
    let executor = fixture.local();
    let polls = Rc::new(Cell::new(0));
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));

    executor.spawn({
        let polls = Rc::clone(&polls);
        let saved_waker = Rc::clone(&saved_waker);
        poll_fn(move |context| {
            polls.set(polls.get() + 1);
            *saved_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        })
    });

    assert_eq!(executor.run_ready_batch().polled(), 1);
    let waker = saved_waker.borrow().as_ref().unwrap().clone();
    waker.wake_by_ref();
    waker.wake_by_ref();
    waker.wake_by_ref();

    assert_eq!(executor.run_ready_batch().polled(), 1);
    assert_eq!(polls.get(), 2);
}

#[test]
fn ready_consumer_defers_detach_while_a_waker_retains_the_observed_head() {
    let fixture = executor();
    let executor = fixture.local();
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));

    executor.spawn({
        let saved_waker = Rc::clone(&saved_waker);
        poll_fn(move |context| {
            *saved_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        })
    });
    assert_eq!(executor.run_ready_batch().polled(), 1);

    executor.spawn(core::future::pending());
    executor.shared.ready.arm_test_publisher_pause();
    let remote_waker = saved_waker.borrow().as_ref().unwrap().clone();
    let publisher = std::thread::spawn(move || remote_waker.wake_by_ref());
    executor.shared.ready.wait_for_test_publisher_pause();

    let while_observed = executor.run_ready_batch();
    executor.shared.ready.resume_test_publisher();
    publisher.join().unwrap();

    let after_publish = executor.run_ready_batch();
    assert_eq!(
        while_observed.polled(),
        0,
        "the owner must not detach a ready head whose address and provenance are still retained \
         by a waker"
    );
    assert!(
        while_observed.has_more(),
        "deferred grace must keep the owner runnable for another bounded batch"
    );
    assert_eq!(after_publish.polled(), 2);
}

#[test]
fn defers_self_wake_until_the_next_batch() {
    let fixture = executor();
    let executor = fixture.local();
    let polls = Rc::new(Cell::new(0));

    executor.spawn({
        let polls = Rc::clone(&polls);
        poll_fn(move |context| {
            let poll = polls.get() + 1;
            polls.set(poll);
            if poll == 1 {
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
    });

    assert_eq!(executor.run_ready_batch().polled(), 1);
    assert_eq!(polls.get(), 1);
    assert!(executor.has_ready());
    assert_eq!(executor.run_ready_batch().polled(), 1);
    assert_eq!(polls.get(), 2);
}

#[test]
fn limits_each_ready_batch_to_sixty_four_polls() {
    let fixture = executor();
    let executor = fixture.local();

    for _ in 0..65 {
        executor.spawn(async {});
    }

    let first = executor.run_ready_batch();
    assert_eq!(first.polled(), DEFAULT_POLL_BATCH);
    assert!(first.has_more());
    assert_eq!(executor.run_ready_batch().polled(), 1);
}

#[test]
fn closes_the_wake_during_park_window() {
    let fixture = executor();
    let executor = fixture.local();
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));

    executor.spawn({
        let saved_waker = Rc::clone(&saved_waker);
        poll_fn(move |context| {
            *saved_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        })
    });
    executor.run_ready_batch();

    let token = executor.prepare_park().expect("executor should be idle");
    saved_waker.borrow().as_ref().unwrap().wake_by_ref();

    assert!(token.finish());
    assert!(executor.has_ready());
}

#[test]
fn direct_wake_retains_from_the_owning_arc_provenance() {
    let fixture = executor();
    let owner_wake = fixture.local().shared.owner_wake.clone();

    assert_eq!(owner_wake.wake(), crate::WakeResult::Notified);
}

#[test]
fn predicate_aware_os_park_observes_work_after_scheduler_wake_drain() {
    let fixture = executor();
    let completed = Rc::new(Cell::new(false));
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));
    let executor = fixture.local();
    let owner_wake = executor.shared.owner_wake.clone();
    let system = fixture.system();

    executor.run(
        {
            let completed = Rc::clone(&completed);
            let saved_waker = Rc::clone(&saved_waker);
            poll_fn(move |context| {
                if completed.get() {
                    return Poll::Ready(());
                }
                *saved_waker.borrow_mut() = Some(context.waker().clone());
                Poll::Pending
            })
        },
        |condition| {
            completed.set(true);
            saved_waker
                .borrow()
                .as_ref()
                .expect("pending future must publish its waker")
                .wake_by_ref();

            assert!(
                !system
                    .consume_wake(&owner_wake)
                    .expect("running owner must consume the direct wake"),
                "a running owner must not be enqueued by a direct wake"
            );
            assert!(
                condition.should_abort(),
                "executor readiness must survive scheduler notification consumption"
            );
        },
    );
}

#[test]
fn reclaims_a_completed_coroutine_only_after_the_last_waker_drop() {
    let fixture = executor();
    let executor = fixture.local();
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));
    let future_drop = Arc::new(AtomicUsize::new(0));

    executor.spawn(StoreWakerThenReady {
        saved_waker: Rc::clone(&saved_waker),
        drop_count: Arc::clone(&future_drop),
    });
    executor.run_ready_batch();

    assert_eq!(future_drop.load(Ordering::Relaxed), 1);
    assert_eq!(executor.reclaim_completed(DEFAULT_RECLAIM_BATCH), 0);
    saved_waker.borrow_mut().take();
    assert_eq!(executor.reclaim_completed(DEFAULT_RECLAIM_BATCH), 1);
}

#[test]
fn shutdown_drops_a_pending_non_send_future_on_the_owner() {
    let mut fixture = executor();
    let future_drop = Rc::new(Cell::new(0));
    fixture.local().spawn(PendingDrop {
        drop_count: Rc::clone(&future_drop),
    });
    fixture.local().run_ready_batch();

    fixture.shutdown();

    assert_eq!(future_drop.get(), 1);
}

#[test]
fn shutdown_reclaims_the_header_when_a_future_destructor_panics() {
    let mut fixture = executor();
    let future_drop = Rc::new(Cell::new(0));
    fixture.local().spawn(PanickingPendingDrop {
        drop_count: Rc::clone(&future_drop),
    });
    fixture.local().run_ready_batch();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fixture.shutdown()));

    assert!(panic.is_err());
    assert_eq!(future_drop.get(), 1);
    assert_eq!(fixture.system().drain_deferred_reclaims(1).unwrap(), 1);
}

#[test]
fn late_waker_is_inert_after_shutdown_and_reaped_by_the_task_system() {
    let mut fixture = executor();
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));
    fixture.local().spawn(StoreWakerThenReady {
        saved_waker: Rc::clone(&saved_waker),
        drop_count: Arc::new(AtomicUsize::new(0)),
    });
    fixture.local().run_ready_batch();
    let late = saved_waker.borrow_mut().take().unwrap();

    fixture.shutdown();
    late.wake_by_ref();
    drop(late);

    assert_eq!(fixture.system().drain_deferred_reclaims(1).unwrap(), 1);
}

#[test]
fn run_supports_a_borrowing_non_send_future_without_leaking_its_header() {
    let fixture = executor();
    let executor = fixture.local();
    let observed = Rc::new(Cell::new(0));
    let borrowed = Rc::clone(&observed);

    let output = executor.run(
        async {
            borrowed.set(7);
            11
        },
        |_| panic!("a ready future must not park"),
    );

    assert_eq!(output, 11);
    assert_eq!(observed.get(), 7);
    assert_eq!(fixture.system().drain_deferred_reclaims(1).unwrap(), 0);
}

#[test]
fn try_run_park_error_cancels_self_woken_future_and_keeps_executor_reusable() {
    let fixture = executor();
    let executor = fixture.local();
    let polls = Rc::new(Cell::new(0));
    let drops = Rc::new(Cell::new(0));

    let result = executor.try_run(
        SelfWakeThenPending {
            polls: Rc::clone(&polls),
            drops: Rc::clone(&drops),
        },
        |_| Err("device owner stopped"),
    );

    assert_eq!(result, Err("device owner stopped"));
    assert_eq!(polls.get(), 2, "the queued self-wake must be consumed once");
    assert_eq!(
        drops.get(),
        1,
        "park failure must drop the future exactly once"
    );
    assert!(!executor.has_ready(), "cancel must remove queued root work");
    assert_eq!(
        executor.reclaim_completed(1),
        0,
        "try_run reclaims its root"
    );

    let next = executor.try_run(async { 17 }, |_| -> Result<(), &str> {
        panic!("a ready replacement future must not park")
    });
    assert_eq!(next, Ok(17), "an aborted root must not poison the executor");
}

#[test]
fn borrowing_root_late_waker_is_safe_after_run_returns() {
    let fixture = executor();
    let executor = fixture.local();
    let stack_value = Cell::new(0);
    let saved_waker = Rc::new(RefCell::new(None::<Waker>));

    let output = executor.run(
        poll_fn(|context| {
            stack_value.set(5);
            *saved_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Ready(13)
        }),
        |_| panic!("a ready future must not park"),
    );

    assert_eq!(output, 13);
    assert_eq!(stack_value.get(), 5);
    assert_eq!(executor.reclaim_completed(1), 0);
    let late = saved_waker.borrow_mut().take().unwrap();
    late.wake_by_ref();
    drop(late);
    assert_eq!(executor.reclaim_completed(1), 1);
}

#[test]
fn poll_panic_cancels_and_drops_a_borrowing_future_on_its_owner() {
    let fixture = executor();
    let executor = fixture.local();
    let borrowed_state = Cell::new(0);
    let dropped_on = Cell::new(None);
    let owner = executor.owner_thread();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.run(
            PanicBorrowingFuture {
                borrowed_state: &borrowed_state,
                dropped_on: &dropped_on,
            },
            |_| panic!("a panicking first poll must not park"),
        )
    }));

    assert!(panic.is_err());
    assert_eq!(borrowed_state.get(), 2);
    assert_eq!(dropped_on.get(), Some(owner));
    assert_eq!(executor.reclaim_completed(1), 1);
}

#[test]
fn task_system_reclaimer_obeys_the_requested_bound() {
    let fixture = executor();
    let executor = fixture.local();
    executor.spawn(async {});
    executor.spawn(async {});
    assert_eq!(executor.run_ready_batch().completed(), 2);

    assert_eq!(executor.reclaim_completed(1), 1);
    assert_eq!(executor.reclaim_completed(1), 1);
    assert_eq!(executor.reclaim_completed(1), 0);
}

#[test]
fn shutdown_waits_for_an_inflight_ready_publisher() {
    let mut fixture = executor();
    let shared = Arc::clone(&fixture.local().shared);
    let publisher = Arc::clone(&shared);
    let entered = Arc::new(std::sync::Barrier::new(2));
    let publisher_entered = Arc::clone(&entered);

    let worker = std::thread::spawn(move || {
        assert!(publisher.begin_ready_publish());
        publisher_entered.wait();
        while publisher.ready_publication.load(Ordering::Acquire) & READY_PUBLICATION_CLOSED == 0 {
            core::hint::spin_loop();
        }
        publisher.finish_ready_publish();
    });

    entered.wait();
    fixture.shutdown();
    worker.join().expect("ready publisher must finish");

    assert!(!shared.begin_ready_publish());
    assert_eq!(Arc::strong_count(&shared), 1);
}

#[test]
fn reference_overflow_reports_a_fatal_invariant_instead_of_becoming_immortal() {
    let fixture = executor();
    let executor = fixture.local();
    executor.spawn(core::future::pending());
    let header = executor.active.get();
    unsafe {
        coroutine::force_reference_count(header, usize::MAX);
    }

    let overflow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        // The permanent owner reference keeps `header` valid for this invariant test.
        coroutine::retain_reference(&*header);
    }));

    assert!(overflow.is_err());
    unsafe {
        // Restore owner + ready references so ordinary shutdown can verify the
        // rest of the lifetime protocol without an artificial leak.
        coroutine::force_reference_count(header, 2);
    }
}

fn executor() -> ExecutorFixture {
    let system =
        Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).expect("task system must initialize"));
    let mut cpu = system
        .create_cpu_local(CpuId::new(0))
        .expect("CPU local must initialize");
    let thread = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .expect("bootstrap thread must initialize");
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("CPU must come online");
    crate::test_runtime::install_task_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        // SAFETY: the fixture keeps this owner object pinned and serializes
        // every scheduler borrow until it clears the runtime handle.
        (unsafe { Pin::get_unchecked_mut(cpu.as_mut()) } as *mut crate::CpuLocal)
            .expose_provenance(),
    );
    let executor =
        LocalExecutor::new(thread.wake_handle()).expect("executor owner identity must match");
    ExecutorFixture {
        executor: Some(executor),
        cpu,
        system,
    }
}

struct ExecutorFixture {
    executor: Option<LocalExecutor>,
    cpu: Pin<Box<crate::CpuLocal>>,
    system: Pin<Box<TaskSystem>>,
}

impl ExecutorFixture {
    fn local(&self) -> &LocalExecutor {
        self.executor.as_ref().expect("executor must remain active")
    }

    fn system(&self) -> &TaskSystem {
        self.system.as_ref().get_ref()
    }

    fn shutdown(&mut self) {
        drop(self.executor.take());
    }

    fn drain_runtime_work(&mut self) {
        loop {
            let batch = self
                .system
                .drain_remote_wakes(self.cpu.as_mut(), 0)
                .expect("test CPU must accept its pending wakes");
            if !batch.pending() {
                break;
            }
        }
        while self
            .system
            .drain_deferred_reclaims(DEFAULT_RECLAIM_BATCH)
            .expect("test reaper must run in task context")
            != 0
        {}
    }
}

impl Drop for ExecutorFixture {
    fn drop(&mut self) {
        self.shutdown();
        self.drain_runtime_work();
        crate::test_runtime::clear_task_handles();
    }
}

struct StoreWakerThenReady {
    saved_waker: Rc<RefCell<Option<Waker>>>,
    drop_count: Arc<AtomicUsize>,
}

impl core::future::Future for StoreWakerThenReady {
    type Output = ();

    fn poll(self: core::pin::Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        *self.saved_waker.borrow_mut() = Some(context.waker().clone());
        Poll::Ready(())
    }
}

impl Drop for StoreWakerThenReady {
    fn drop(&mut self) {
        self.drop_count.fetch_add(1, Ordering::Relaxed);
    }
}

struct PendingDrop {
    drop_count: Rc<Cell<usize>>,
}

struct SelfWakeThenPending {
    polls: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
}

impl Future for SelfWakeThenPending {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let polls = self.polls.get() + 1;
        self.polls.set(polls);
        if polls == 1 {
            context.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl Drop for SelfWakeThenPending {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

struct PanickingPendingDrop {
    drop_count: Rc<Cell<usize>>,
}

struct PanicBorrowingFuture<'borrow> {
    borrowed_state: &'borrow Cell<usize>,
    dropped_on: &'borrow Cell<Option<crate::ThreadId>>,
}

impl Future for PanicBorrowingFuture<'_> {
    type Output = ();

    fn poll(self: core::pin::Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        self.borrowed_state.set(1);
        panic!("poll failure used to verify scoped cancellation")
    }
}

impl Drop for PanicBorrowingFuture<'_> {
    fn drop(&mut self) {
        self.borrowed_state.set(2);
        self.dropped_on.set(Some(
            crate::current_thread_id().expect("future drop must run on a scheduler thread"),
        ));
    }
}

impl Future for PendingDrop {
    type Output = ();

    fn poll(self: core::pin::Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

impl Drop for PendingDrop {
    fn drop(&mut self) {
        self.drop_count.set(self.drop_count.get() + 1);
    }
}

impl Future for PanickingPendingDrop {
    type Output = ();

    fn poll(self: core::pin::Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

impl Drop for PanickingPendingDrop {
    fn drop(&mut self) {
        self.drop_count.set(self.drop_count.get() + 1);
        panic!("future destructor failure used to verify unwind-safe reclamation");
    }
}

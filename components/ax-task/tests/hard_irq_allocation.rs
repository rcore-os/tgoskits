//! Allocation audit for operations permitted in hard interrupt context.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    boxed::Box,
    cell::{Cell, RefCell},
    pin::Pin,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};

use ax_task::{
    CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadId, ThreadSpec,
    executor::{DEFAULT_RECLAIM_BATCH, LocalExecutor},
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    timer::{ExpireRequest, ExpiredTimer, TimerNode, TimerQueue},
};

mod support;

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static AUDIT_ENABLED: Cell<bool> = const { Cell::new(false) };
}

struct AuditAllocator;

// SAFETY: every operation delegates to `System` with the original layout and
// pointer. The counters are observational and thread-local gating excludes the
// test harness and unrelated test threads.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && audit_enabled() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && audit_enabled() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if audit_enabled() {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() && audit_enabled() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        replacement
    }
}

#[test]
fn hard_irq_contract_is_zero_alloc_zero_free_and_zero_poll() {
    support::clear_handles();
    assert_eq!(support::last_oneshot_ns(), 0);
    let system =
        Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).expect("task system must initialize"));
    let system_ref = system.as_ref().get_ref();
    let mut cpu = system_ref
        .create_cpu_local(CpuId::new(0))
        .expect("CPU local must initialize");
    let executor_thread = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .expect("bootstrap thread must initialize");
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("CPU must come online");
    support::install_handles(
        (system_ref as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .expect("thread must initialize");
    system
        .make_ready(thread.id())
        .expect("thread must be ready");
    system
        .enqueue(cpu.as_mut(), thread.id(), 0)
        .expect("thread must be queued");
    let wake = thread.wake_handle();

    support::set_hard_irq(true);
    let thread_wake_audit = audit(|| wake.wake());
    assert_zero_allocator_activity(thread_wake_audit);
    assert_eq!(support::ipi_count(0), 1);

    let inbox = SchedulerInbox::new(InboxKind::RemoteWake);
    let inbox_node = OwnedInboxNode::new(InboxKind::RemoteWake);
    let inbox_audit = audit(|| {
        inbox.publish(
            inbox_node.pinned(),
            InboxMessage::remote_wake(ThreadId::from_parts(7, 1), CpuId::new(0)),
        )
    });
    assert_eq!(inbox_audit.value, PublishResult::Published);
    assert_zero_allocator_activity(inbox_audit);

    let mut timer_queue = TimerQueue::new(1);
    let timer = Box::pin(TimerNode::new(11));
    unsafe {
        timer_queue
            .arm(timer.as_ref(), 10)
            .expect("preallocated timer slot must be available");
    }
    let mut expired = [ExpiredTimer::EMPTY; 1];
    let timer_audit = audit(|| timer_queue.expire(ExpireRequest::new(10, 1, 1), &mut expired));
    assert_eq!(timer_audit.value.expired(), 1);
    assert_zero_allocator_activity(timer_audit);
    support::set_hard_irq(false);

    let executor = LocalExecutor::new(executor_thread.wake_handle())
        .expect("executor owner identity must match");
    let polls = Rc::new(Cell::new(0));
    let pending_waker = Rc::new(RefCell::new(None::<Waker>));
    executor.spawn({
        let polls = Rc::clone(&polls);
        let pending_waker = Rc::clone(&pending_waker);
        core::future::poll_fn(move |context| {
            polls.set(polls.get() + 1);
            *pending_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        })
    });
    let late_waker = Rc::new(RefCell::new(None::<Waker>));
    executor.spawn({
        let late_waker = Rc::clone(&late_waker);
        core::future::poll_fn(move |context| {
            *late_waker.borrow_mut() = Some(context.waker().clone());
            Poll::Ready(())
        })
    });
    let first_batch = executor.run_ready_batch();
    assert_eq!(first_batch.polled(), 2);
    assert_eq!(first_batch.completed(), 1);
    let pending = pending_waker
        .borrow_mut()
        .take()
        .expect("pending future must publish its raw waker");
    let late = late_waker
        .borrow_mut()
        .take()
        .expect("completed future must publish its raw waker");
    let polls_before_irq_ops = polls.get();

    support::set_hard_irq(true);
    let raw_waker_audit = audit(|| {
        let borrowed = pending.clone();
        borrowed.wake_by_ref();
        let cloned = pending.clone();
        cloned.wake();
        drop(borrowed);
        drop(pending);

        let borrowed_late = late.clone();
        borrowed_late.wake_by_ref();
        drop(borrowed_late);
        drop(late);
    });
    assert_zero_allocator_activity(raw_waker_audit);
    assert_eq!(
        polls.get(),
        polls_before_irq_ops,
        "wake must not poll a future"
    );
    let hard_irq_reclaim_audit = audit(|| system.drain_deferred_reclaims(1));
    assert_eq!(
        hard_irq_reclaim_audit.value,
        Err(ax_task::TaskError::UnsafeContext)
    );
    assert_zero_allocator_activity(hard_irq_reclaim_audit);
    support::set_hard_irq(false);
    assert_eq!(executor.reclaim_completed(DEFAULT_RECLAIM_BATCH), 1);

    let mut inbox_output = [InboxMessage::EMPTY; 1];
    assert_eq!(inbox.drain(1, &mut inbox_output).drained(), 1);
    system
        .drain_remote_wakes(cpu.as_mut(), 0)
        .expect("owner must consume the direct wake reference");
    drop(executor);
    support::clear_handles();

    // Keep teardown explicit so default Miri leak checking verifies the same
    // fixture that audits hard-IRQ allocator activity. Both intrusive nodes
    // have been detached from their owner queues before their storage drops.
    drop(timer_queue);
    drop(timer);
    drop(inbox_node);
    drop(wake);
    drop(thread);
    drop(executor_thread);
    drop(cpu);
    drop(system);
}

fn audit<T>(operation: impl FnOnce() -> T) -> AuditResult<T> {
    AUDIT_ENABLED.with(|enabled| {
        assert!(!enabled.get(), "allocator audits must not be nested");
        ALLOCATIONS.store(0, Ordering::Relaxed);
        DEALLOCATIONS.store(0, Ordering::Relaxed);
        enabled.set(true);
        let value = operation();
        enabled.set(false);
        AuditResult {
            value,
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        }
    })
}

fn audit_enabled() -> bool {
    AUDIT_ENABLED.try_with(Cell::get).unwrap_or(false)
}

fn assert_zero_allocator_activity<T>(audit: AuditResult<T>) {
    assert_eq!(audit.allocations, 0, "hard IRQ operation allocated");
    assert_eq!(audit.deallocations, 0, "hard IRQ operation freed memory");
}

struct AuditResult<T> {
    value: T,
    allocations: usize,
    deallocations: usize,
}

struct OwnedInboxNode {
    node: *mut InboxNode,
}

impl OwnedInboxNode {
    fn new(kind: InboxKind) -> Self {
        Self {
            node: Box::into_raw(Box::new(InboxNode::new(kind))),
        }
    }

    fn pinned(&self) -> Pin<&'static InboxNode> {
        // SAFETY: `node` comes from Box, remains pinned until this owner's Drop,
        // and the test drops the drained SchedulerInbox before this owner.
        unsafe { Pin::new_unchecked(&*self.node) }
    }
}

impl Drop for OwnedInboxNode {
    fn drop(&mut self) {
        // SAFETY: the test drains and drops the only inbox that observed this
        // pointer before dropping the owner, and no producer retains it.
        unsafe { drop(Box::from_raw(self.node)) };
    }
}

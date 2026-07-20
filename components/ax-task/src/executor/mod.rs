//! Single-owner coroutine execution with interrupt-safe wake publication.
//!
//! Futures are polled and destroyed only by the owner thread. Wakers may cross
//! CPUs and may run in hard interrupt context; their operations only touch
//! atomics, publish intrusive nodes, and invoke a direct thread wake header.

mod coroutine;
mod inbox;
mod waker;

use alloc::{boxed::Box, rc::Rc, sync::Arc};
use core::{
    cell::{Cell, RefCell},
    fmt,
    future::Future,
    marker::PhantomData,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

pub use coroutine::{CoroutineHeader, CoroutineId};

use self::{
    coroutine::{COMPLETE, Coroutine, POLLING, RUN_QUEUED, release_reference, retain_reference},
    inbox::{InboxKind, IntrusiveInbox},
    waker::coroutine_waker,
};
use crate::{
    TaskError, ThreadId, ThreadWakeHandle,
    runtime::{IrqGuardToken, task_runtime},
};

/// Maximum number of futures polled by one executor turn.
pub const DEFAULT_POLL_BATCH: usize = 64;

/// Default bound for task-context deferred reclamation.
pub const DEFAULT_RECLAIM_BATCH: usize = 64;

const NOTIFIED: usize = 1 << 0;
const PARKING: usize = 1 << 1;
const PARKED: usize = 1 << 2;

/// A single-thread executor whose owner may migrate between CPUs.
///
/// The executor is deliberately `!Send` and `!Sync`: only its scheduler thread
/// may spawn, poll, park, or shut down futures. Its heap-pinned shared header is
/// retained by coroutine allocations, so late wakers never address the local
/// owner object after it has been dropped.
pub struct LocalExecutor {
    shared: Arc<SharedExecutor>,
    ready_pending: Cell<*mut CoroutineHeader>,
    active: Cell<*mut CoroutineHeader>,
    next_generation: Cell<u64>,
    _owner_thread_only: PhantomData<Rc<()>>,
}

impl LocalExecutor {
    /// Creates an executor for the calling scheduler thread.
    ///
    /// The owner identity is derived from the direct wake header rather than a
    /// caller-supplied integer, and is checked against the currently running
    /// scheduler thread.
    ///
    /// # Errors
    ///
    /// Returns a scheduler facade error before runtime initialization, or
    /// [`TaskError::ExecutorOwnerMismatch`] when `owner_wake` belongs to another
    /// thread.
    pub fn new(owner_wake: ThreadWakeHandle) -> Result<Self, TaskError> {
        if crate::runtime::task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let expected = owner_wake.thread_id();
        let actual = crate::current_thread_id()?;
        if actual != expected {
            return Err(TaskError::ExecutorOwnerMismatch {
                expected: expected.as_u64(),
                actual: actual.as_u64(),
            });
        }
        Ok(Self {
            shared: Arc::new(SharedExecutor::new(owner_wake)),
            ready_pending: Cell::new(ptr::null_mut()),
            active: Cell::new(ptr::null_mut()),
            next_generation: Cell::new(1),
            _owner_thread_only: PhantomData,
        })
    }

    /// Returns the scheduler thread that owns this executor.
    pub fn owner_thread(&self) -> ThreadId {
        self.shared.owner_thread
    }

    /// Allocates and schedules a `'static` coroutine for the owner thread.
    ///
    /// `future` need not implement `Send`; it is polled and dropped only by the
    /// executor owner. Allocation happens here in ordinary thread context, never
    /// in a waker operation.
    pub fn spawn<F>(&self, future: F) -> CoroutineId
    where
        F: Future<Output = ()> + 'static,
    {
        self.assert_owner_context();
        unsafe {
            // A static future may remain active until explicit executor shutdown.
            self.spawn_scoped(future).1
        }
    }

    /// Runs one possibly borrowing future to completion on the owner thread.
    ///
    /// `park` is called only after the executor-side lost-wake handshake has
    /// completed. It must pass the supplied [`ExecutorParkCondition`] into the
    /// OS scheduler's predicate-aware park operation. Checking the condition
    /// before an unconditional park is insufficient because scheduler wake
    /// consumption may race between those two operations. The future is dropped
    /// on the owner before this method returns or unwinds.
    pub fn run<F, P>(&self, future: F, mut park: P) -> F::Output
    where
        F: Future,
        P: FnMut(&ExecutorParkCondition<'_>),
    {
        match self.try_run(future, |condition| {
            park(condition);
            Ok::<(), core::convert::Infallible>(())
        }) {
            Ok(output) => output,
            Err(never) => match never {},
        }
    }

    /// Runs one borrowing future until completion or a fallible OS park aborts.
    ///
    /// An error returned by `park` cancels and drops the root future on this
    /// owner thread, removes any queued self-wake, and reclaims the root header
    /// before returning. Device maintenance owners use this to stop a future
    /// when IRQ service or source rearm can no longer make safe progress.
    pub fn try_run<F, P, E>(&self, future: F, mut park: P) -> Result<F::Output, E>
    where
        F: Future,
        P: FnMut(&ExecutorParkCondition<'_>) -> Result<(), E>,
    {
        self.assert_owner_context();
        let output = RefCell::new(None);
        let root = async {
            output.replace(Some(future.await));
        };
        let (header, _) = unsafe {
            // `ScopedRunGuard` cancels and empties this borrowing future before
            // `output` and the caller's borrowed data can leave this stack.
            self.spawn_scoped(root)
        };
        retain_reference(unsafe {
            // The fresh allocation is live through its permanent owner reference.
            &*header
        });
        let guard = ScopedRunGuard {
            executor: self,
            header,
        };

        while output.borrow().is_none() {
            let batch = self.run_ready_batch();
            if output.borrow().is_some() || batch.has_more() {
                continue;
            }
            let Some(token) = self.prepare_park() else {
                continue;
            };
            let condition = ExecutorParkCondition { executor: self };
            if let Err(error) = park(&condition) {
                drop(token);
                drop(guard);
                let _reclaimed = self.reclaim_completed(DEFAULT_RECLAIM_BATCH);
                return Err(error);
            }
            let _owner_work = token.finish();
            unsafe {
                // Returning from the OS park path is also a reason to recheck a
                // root future for signal or non-executor readiness changes.
                coroutine::schedule(header);
            }
        }

        let result = output
            .borrow_mut()
            .take()
            .unwrap_or_else(|| unreachable!("completed root future must publish output"));
        drop(guard);
        let _reclaimed = self.reclaim_completed(DEFAULT_RECLAIM_BATCH);
        Ok(result)
    }

    /// Polls at most 64 ready coroutines from one queue snapshot.
    ///
    /// Wakes produced while a future is being polled are published to the next
    /// snapshot, so a self-waking future cannot consume the current batch.
    pub fn run_ready_batch(&self) -> PollBatch {
        self.assert_owner_context();
        self.shared
            .park_state
            .fetch_and(!NOTIFIED, Ordering::AcqRel);
        let mut cursor = self.take_ready_snapshot();
        let mut polled = 0;
        let mut completed = 0;

        while !cursor.is_null() && polled < DEFAULT_POLL_BATCH {
            let header = cursor;
            cursor = unsafe {
                // The ready queue reference keeps `header` alive. Only the owner
                // consumes this detached list and may rewrite its next pointer.
                IntrusiveInbox::take_next(header, InboxKind::Ready)
            };
            let did_complete = unsafe {
                // Detached ready nodes are uniquely owned by this executor turn.
                self.poll_ready_coroutine(header)
            };
            polled += usize::from(did_complete.was_polled());
            completed += usize::from(did_complete.was_completed());
        }

        self.ready_pending.set(cursor);
        PollBatch {
            polled,
            completed,
            has_more: self.has_ready(),
        }
    }

    /// Runs a bounded task-system pass over zero-reference coroutine headers.
    ///
    /// Future destructors have already run on their owner. This pass may free
    /// headers whose last waker was dropped by any CPU or hard interrupt.
    pub fn reclaim_completed(&self, limit: usize) -> usize {
        self.assert_owner_context();
        crate::facade::drain_deferred_reclaims(limit).unwrap_or(0)
    }

    /// Reports whether this executor has a coroutine ready for a future batch.
    pub fn has_ready(&self) -> bool {
        !self.ready_pending.get().is_null() || !self.shared.ready.is_empty()
    }

    /// Begins the `NOTIFIED/PARKING/PARKED` lost-wake handshake.
    ///
    /// The caller must hold the returned token across the task-system park
    /// operation. A wake after this function succeeds observes `PARKED`, publishes
    /// `NOTIFIED`, and wakes the owner through its direct thread wake header.
    pub fn prepare_park(&self) -> Option<ParkToken<'_>> {
        self.assert_owner_context();
        if self.has_owner_work() {
            return None;
        }

        let previous = self.shared.park_state.fetch_or(PARKING, Ordering::AcqRel);
        if previous & (NOTIFIED | PARKING | PARKED) != 0 || self.has_owner_work() {
            self.cancel_park_attempt();
            return None;
        }

        if self
            .shared
            .park_state
            .compare_exchange(PARKING, PARKED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
            || self.has_owner_work()
        {
            self.cancel_park_attempt();
            return None;
        }

        Some(ParkToken {
            executor: self,
            active: true,
            _owner_thread_only: PhantomData,
        })
    }

    unsafe fn spawn_scoped<F>(&self, future: F) -> (*mut CoroutineHeader, CoroutineId)
    where
        F: Future<Output = ()>,
    {
        let id = self.allocate_coroutine_id();
        let coroutine = Box::new(Coroutine::new(id, Arc::clone(&self.shared), future));
        let header = Box::into_raw(coroutine).cast::<CoroutineHeader>();
        self.link_active(header);
        unsafe {
            // The fresh pinned allocation owns its permanent owner reference;
            // publication retains a distinct ready-queue reference.
            coroutine::schedule(header);
        }
        (header, id)
    }

    fn allocate_coroutine_id(&self) -> CoroutineId {
        let generation = self.next_generation.get();
        self.next_generation.set(generation.wrapping_add(1).max(1));
        CoroutineId::new(self.shared.owner_thread, generation)
    }

    fn take_ready_snapshot(&self) -> *mut CoroutineHeader {
        let pending = self.ready_pending.replace(ptr::null_mut());
        if pending.is_null() {
            unsafe {
                // This executor is the only consumer of its ready inbox.
                self.shared.ready.take_fifo()
            }
        } else {
            pending
        }
    }

    /// Polls one node detached from the ready inbox.
    ///
    /// # Safety
    ///
    /// `header` must carry a live ready-queue reference and belong to this
    /// executor's exclusively owned detached snapshot.
    unsafe fn poll_ready_coroutine(&self, header: *mut CoroutineHeader) -> PollDisposition {
        let state = unsafe {
            // The queue reference guarantees a valid header throughout polling.
            &(*header).state
        };
        let dequeued_state = state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
        let mut queue_reference = ReadyQueueReference::new(header);
        if dequeued_state & COMPLETE != 0 {
            return PollDisposition::Skipped;
        }

        state.fetch_or(POLLING, Ordering::AcqRel);
        queue_reference.mark_polling();
        let waker = unsafe {
            // `header` remains pinned and the queue reference outlives the Waker.
            coroutine_waker(header)
        };
        let mut context = Context::from_waker(&waker);
        let result = unsafe {
            // Only the owner reaches this function, and POLLING excludes a second
            // owner poll of the same future.
            CoroutineHeader::poll_raw(header, &mut context)
        };
        drop(waker);
        queue_reference.finish_polling();

        match result {
            Poll::Pending => PollDisposition::Pending,
            Poll::Ready(()) => {
                self.complete_coroutine(header);
                PollDisposition::Completed
            }
        }
    }

    fn complete_coroutine(&self, header: *mut CoroutineHeader) {
        let state = unsafe {
            // The owner holds both the permanent and current queue references.
            &(*header).state
        };
        state.fetch_or(COMPLETE, Ordering::AcqRel);
        self.unlink_active(header);
        let _owner_reference = unsafe {
            // Completion consumes the permanent owner reference even if the
            // owner-only future destructor unwinds.
            OwnedCoroutineReference::new(header)
        };
        unsafe {
            // Completion destroys the !Send future on its owner before dropping
            // the permanent owner reference.
            CoroutineHeader::drop_future_raw(header);
        }
    }

    fn cancel_coroutine(&self, header: *mut CoroutineHeader) {
        let state = unsafe {
            // ScopedRunGuard owns a live reference and cancellation is owner-only.
            &(*header).state
        };
        if state.fetch_or(COMPLETE, Ordering::AcqRel) & COMPLETE != 0 {
            return;
        }
        self.unlink_active(header);
        let _owner_reference = unsafe {
            // Cancellation consumes the permanent owner reference even if the
            // owner-only future destructor unwinds.
            OwnedCoroutineReference::new(header)
        };
        unsafe {
            // Cancellation follows the same owner-only destructor ordering as
            // normal completion, but a queued reference may remain for later skip.
            CoroutineHeader::drop_future_raw(header);
        }
    }

    fn link_active(&self, header: *mut CoroutineHeader) {
        unsafe {
            // Only the owner mutates the active list. The allocation's permanent
            // reference keeps every linked header alive.
            (*header).set_owner_next(self.active.get());
        }
        self.active.set(header);
    }

    fn unlink_active(&self, header: *mut CoroutineHeader) {
        let mut previous = ptr::null_mut::<CoroutineHeader>();
        let mut cursor = self.active.get();
        while !cursor.is_null() {
            let next = unsafe {
                // The permanent owner reference keeps active-list nodes live.
                (*cursor).owner_next()
            };
            if cursor == header {
                if previous.is_null() {
                    self.active.set(next);
                } else {
                    unsafe {
                        // Owner-only list mutation cannot overlap a second unlink.
                        (*previous).set_owner_next(next);
                    }
                }
                unsafe {
                    // A detached node must not retain a stale owner-list link.
                    (*cursor).set_owner_next(ptr::null_mut());
                }
                return;
            }
            previous = cursor;
            cursor = next;
        }
    }

    fn has_owner_work(&self) -> bool {
        self.has_ready()
    }

    fn cancel_park_attempt(&self) {
        self.shared
            .park_state
            .fetch_and(!(PARKING | PARKED | NOTIFIED), Ordering::AcqRel);
    }

    fn finish_park(&self) -> bool {
        let state = self.shared.park_state.swap(0, Ordering::AcqRel);
        state & NOTIFIED != 0 || self.has_owner_work()
    }

    fn assert_owner_context(&self) {
        if crate::runtime::task_runtime::in_hard_irq() {
            crate::runtime::task_runtime::fatal_invariant(
                0x4558_0005,
                self.owner_thread().as_u64() as usize,
            );
        }
        match crate::current_thread_id() {
            Ok(actual) if actual == self.owner_thread() => {}
            Ok(actual) => {
                crate::runtime::task_runtime::fatal_invariant(0x4558_0003, actual.as_u64() as usize)
            }
            Err(_) => crate::runtime::task_runtime::fatal_invariant(
                0x4558_0006,
                self.owner_thread().as_u64() as usize,
            ),
        }
    }

    fn shutdown(&mut self) {
        self.shared.close_and_wait_for_publishers();
        self.discard_ready_references();

        let mut cursor = self.active.replace(ptr::null_mut());
        while !cursor.is_null() {
            let header = cursor;
            cursor = unsafe {
                // The permanent owner reference keeps the detached active list
                // live until each future and reference are released below.
                (*header).owner_next()
            };
            unsafe {
                (*header).set_owner_next(ptr::null_mut());
            }
            let state = unsafe { &(*header).state };
            if state.fetch_or(COMPLETE, Ordering::AcqRel) & COMPLETE == 0 {
                let _owner_reference = unsafe {
                    // The detached active node transfers its permanent owner
                    // reference to this unwind-safe scope guard.
                    OwnedCoroutineReference::new(header)
                };
                unsafe {
                    // LocalExecutor is !Send, so shutdown runs on the owner Rust
                    // thread and is the final path that may destroy !Send futures.
                    CoroutineHeader::drop_future_raw(header);
                }
            }
        }
        let _reclaimed = self.reclaim_completed(DEFAULT_RECLAIM_BATCH);
    }

    fn discard_ready_references(&self) {
        let mut cursor = self.take_ready_snapshot();
        while !cursor.is_null() {
            let header = cursor;
            cursor = unsafe {
                // Closing waited for every pre-close publisher, so this detached
                // list is the final shared ready snapshot.
                IntrusiveInbox::take_next(header, InboxKind::Ready)
            };
            unsafe {
                (*header).state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
                release_reference(header);
            }
        }
    }
}

/// Borrowed executor predicate for one OS scheduler park attempt.
///
/// The OS adapter must evaluate [`Self::should_abort`] from inside its own
/// generation-checked park handshake. The predicate performs only owner-local
/// reads and atomic observations; it does not poll futures or invoke callbacks.
pub struct ExecutorParkCondition<'executor> {
    executor: &'executor LocalExecutor,
}

impl ExecutorParkCondition<'_> {
    /// Reports whether executor work or a wake publication must cancel the OS park.
    ///
    /// This operation is bounded, non-blocking, and scheduler-non-reentrant, so
    /// an OS adapter may call it from an IRQ-disabled wait-queue predicate.
    pub fn should_abort(&self) -> bool {
        self.executor.shared.park_state.load(Ordering::Acquire) & NOTIFIED != 0
            || self.executor.has_owner_work()
    }
}

impl fmt::Debug for ExecutorParkCondition<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutorParkCondition")
            .field("owner_thread", &self.executor.owner_thread())
            .field("should_abort", &self.should_abort())
            .finish()
    }
}

impl Drop for LocalExecutor {
    fn drop(&mut self) {
        self.assert_owner_context();
        self.shutdown();
    }
}

/// Outcome of one bounded executor turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollBatch {
    polled: usize,
    completed: usize,
    has_more: bool,
}

impl PollBatch {
    /// Returns how many futures were polled.
    pub const fn polled(self) -> usize {
        self.polled
    }

    /// Returns how many futures completed.
    pub const fn completed(self) -> usize {
        self.completed
    }

    /// Reports whether another bounded turn has ready work.
    pub const fn has_more(self) -> bool {
        self.has_more
    }
}

/// Proof that the owner completed the executor-side park handshake.
///
/// Hold this value while attempting to block the owner in the task system. Its
/// drop closes the handshake even when the park attempt is cancelled.
#[must_use = "the token must be held across the task-system park operation"]
pub struct ParkToken<'executor> {
    executor: &'executor LocalExecutor,
    active: bool,
    _owner_thread_only: PhantomData<Rc<()>>,
}

impl ParkToken<'_> {
    /// Finishes a park attempt and reports whether a wake or work was observed.
    pub fn finish(mut self) -> bool {
        self.active = false;
        self.executor.finish_park()
    }
}

impl Drop for ParkToken<'_> {
    fn drop(&mut self) {
        if self.active {
            self.executor.cancel_park_attempt();
        }
    }
}

pub(super) struct SharedExecutor {
    owner_thread: ThreadId,
    owner_wake: ThreadWakeHandle,
    ready: IntrusiveInbox,
    park_state: AtomicUsize,
    ready_publication: AtomicUsize,
}

const READY_PUBLICATION_CLOSED: usize = 1usize << (usize::BITS - 1);
const READY_PUBLISHER_COUNT_MASK: usize = READY_PUBLICATION_CLOSED - 1;

impl SharedExecutor {
    fn new(owner_wake: ThreadWakeHandle) -> Self {
        Self {
            owner_thread: owner_wake.thread_id(),
            owner_wake,
            ready: IntrusiveInbox::new(InboxKind::Ready),
            park_state: AtomicUsize::new(0),
            ready_publication: AtomicUsize::new(0),
        }
    }

    pub(super) fn publish_ready(&self, header: *mut CoroutineHeader) -> bool {
        let Some(_publisher) = self.begin_ready_publish_guard() else {
            return false;
        };
        unsafe {
            // RUN_QUEUED gives this node exclusive ready-list membership and its
            // queue reference keeps the allocation alive until consumption.
            self.ready.push(header);
        }
        self.notify_owner();
        true
    }

    fn begin_ready_publish_guard(&self) -> Option<ReadyPublishGuard<'_>> {
        let irq_token = task_runtime::irq_guard_enter();
        if self.begin_ready_publish() {
            Some(ReadyPublishGuard {
                executor: self,
                irq_token,
                _not_send: PhantomData,
            })
        } else {
            // SAFETY: this consumes the token created above on the same CPU;
            // no publication guard escaped the failed closed-state check.
            unsafe { task_runtime::irq_guard_exit(irq_token) };
            None
        }
    }

    fn begin_ready_publish(&self) -> bool {
        let mut state = self.ready_publication.load(Ordering::Acquire);
        loop {
            if state & READY_PUBLICATION_CLOSED != 0 {
                return false;
            }
            if state & READY_PUBLISHER_COUNT_MASK == READY_PUBLISHER_COUNT_MASK {
                crate::runtime::task_runtime::fatal_invariant(
                    0x4558_0007,
                    self.owner_thread.as_u64() as usize,
                );
            }
            match self.ready_publication.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => state = updated,
            }
        }
    }

    fn finish_ready_publish(&self) {
        let previous = self.ready_publication.fetch_sub(1, Ordering::Release);
        debug_assert_ne!(previous & READY_PUBLISHER_COUNT_MASK, 0);
    }

    fn notify_owner(&self) {
        let previous = self.park_state.fetch_or(NOTIFIED, Ordering::AcqRel);
        if previous & PARKED != 0 {
            let _result = self.owner_wake.wake();
        }
    }

    fn close_and_wait_for_publishers(&self) {
        self.ready_publication
            .fetch_or(READY_PUBLICATION_CLOSED, Ordering::AcqRel);
        while self.ready_publication.load(Ordering::Acquire) != READY_PUBLICATION_CLOSED {
            core::hint::spin_loop();
        }
    }
}

struct ReadyPublishGuard<'executor> {
    executor: &'executor SharedExecutor,
    irq_token: IrqGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for ReadyPublishGuard<'_> {
    fn drop(&mut self) {
        self.executor.finish_ready_publish();
        // SAFETY: construction received this token on the current CPU, the
        // !Send marker prevents migration, and Drop consumes it exactly once.
        unsafe { task_runtime::irq_guard_exit(self.irq_token) };
    }
}

struct ScopedRunGuard<'executor> {
    executor: &'executor LocalExecutor,
    header: *mut CoroutineHeader,
}

struct ReadyQueueReference {
    header: *mut CoroutineHeader,
    polling: bool,
}

struct OwnedCoroutineReference {
    header: *mut CoroutineHeader,
}

impl OwnedCoroutineReference {
    /// Takes ownership of one existing allocation reference.
    ///
    /// # Safety
    ///
    /// The caller must transfer exactly one live reference and must not release
    /// that reference through another path after construction.
    unsafe fn new(header: *mut CoroutineHeader) -> Self {
        Self { header }
    }
}

impl Drop for OwnedCoroutineReference {
    fn drop(&mut self) {
        unsafe {
            // This guard owns exactly the reference transferred at construction.
            release_reference(self.header);
        }
    }
}

impl ReadyQueueReference {
    const fn new(header: *mut CoroutineHeader) -> Self {
        Self {
            header,
            polling: false,
        }
    }

    fn mark_polling(&mut self) {
        self.polling = true;
    }

    fn finish_polling(&mut self) {
        unsafe {
            // Only the owner mutates POLLING, and the queue reference keeps the
            // header live until this guard is dropped.
            (*self.header).state.fetch_and(!POLLING, Ordering::AcqRel);
        }
        self.polling = false;
    }
}

impl Drop for ReadyQueueReference {
    fn drop(&mut self) {
        if self.polling {
            unsafe {
                // A panicking poll has fully unwound before this guard runs; clear
                // the owner-only poll marker before scoped cancellation proceeds.
                (*self.header).state.fetch_and(!POLLING, Ordering::AcqRel);
            }
        }
        unsafe {
            // This guard owns the reference transferred by ready publication,
            // including on poll panic and early-complete skip paths.
            release_reference(self.header);
        }
    }
}

impl Drop for ScopedRunGuard<'_> {
    fn drop(&mut self) {
        let _scoped_reference = unsafe {
            // The scoped guard's independent reference must survive cancellation
            // and be released even if the future destructor unwinds.
            OwnedCoroutineReference::new(self.header)
        };
        self.executor.cancel_coroutine(self.header);
    }
}

#[derive(Clone, Copy)]
enum PollDisposition {
    Skipped,
    Pending,
    Completed,
}

impl PollDisposition {
    const fn was_polled(self) -> bool {
        !matches!(self, Self::Skipped)
    }

    const fn was_completed(self) -> bool {
        matches!(self, Self::Completed)
    }
}

#[cfg(test)]
mod tests;

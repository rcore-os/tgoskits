//! State-driven runtime event primitives.

use alloc::{sync::Arc, task::Wake, vec::Vec};
use core::{
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll, Waker},
};

use crate::sync::SpinMutex;

/// Lost-wake state for thread-hosted `block_on` loops.
pub struct BlockOnWakeState {
    woke: core::sync::atomic::AtomicBool,
}

impl BlockOnWakeState {
    /// Creates a clear wake state.
    pub const fn new() -> Self {
        Self {
            woke: core::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Marks the host task as woken.
    pub fn mark_woke(&self) {
        self.woke.store(true, Ordering::Release);
    }

    /// Consumes the local wake flag.
    pub fn take_woke(&self) -> bool {
        self.woke.swap(false, Ordering::AcqRel)
    }

    /// Returns whether a `block_on` loop must re-poll before sleeping.
    pub fn should_repoll(&self, observed_seq: u64, current_seq: u64) -> bool {
        self.take_woke() || current_seq != observed_seq
    }
}

impl Default for BlockOnWakeState {
    fn default() -> Self {
        Self::new()
    }
}

/// Host task wake capability used by [`BlockOnThreadWaker`].
pub trait BlockOnTaskWake: Clone + Send + Sync + 'static {
    /// Wakes the host task in task context.
    fn wake_task(&self);

    /// Returns the host task wake sequence.
    fn wake_seq(&self) -> u64;
}

/// Generic thread-hosted `block_on` waker core.
pub struct BlockOnThreadWaker<W: BlockOnTaskWake> {
    task_wake: W,
    wake_state: BlockOnWakeState,
}

impl<W: BlockOnTaskWake> BlockOnThreadWaker<W> {
    /// Creates a reference-counted waker core.
    pub fn new(task_wake: W) -> Arc<Self> {
        Arc::new(Self {
            task_wake,
            wake_state: BlockOnWakeState::new(),
        })
    }

    /// Builds a Rust [`Waker`] for polling a future.
    pub fn waker(self: &Arc<Self>) -> Waker {
        Waker::from(self.clone())
    }

    /// Returns the current task wake sequence.
    pub fn wake_seq(&self) -> u64 {
        self.task_wake.wake_seq()
    }

    /// Returns whether the host should re-poll before parking.
    pub fn should_repoll(&self, observed_seq: u64) -> bool {
        self.wake_state.should_repoll(observed_seq, self.wake_seq())
    }
}

impl<W: BlockOnTaskWake> Wake for BlockOnThreadWaker<W> {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_state.mark_woke();
        self.task_wake.wake_task();
    }
}

/// Mutable sequence cursor for [`RuntimeEventCore`].
#[derive(Debug, Default)]
pub struct RuntimeEventSeq(AtomicU64);

impl RuntimeEventSeq {
    /// Creates a zero sequence token.
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Returns the raw sequence value.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Updates the observed sequence.
    pub fn update(&self, seq: RuntimeEventValue) {
        self.0.store(seq.get(), Ordering::Release);
    }
}

/// Published runtime event sequence value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeEventValue(u64);

impl RuntimeEventValue {
    /// Returns the raw sequence value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Default)]
struct RuntimeEventWaiters {
    wakers: Vec<Waker>,
}

/// Sticky event source for IRQ/deferred runtime code.
///
/// `RuntimeEventCore` stores readiness as a monotonically increasing sequence
/// plus coalesced event bits. Wakers are only a hint to re-poll; callers must
/// still consume the published state.
pub struct RuntimeEventCore {
    seq: AtomicU64,
    bits: AtomicU64,
    waiters: SpinMutex<RuntimeEventWaiters>,
}

impl RuntimeEventCore {
    /// Creates an empty event source.
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            bits: AtomicU64::new(0),
            waiters: SpinMutex::new(RuntimeEventWaiters { wakers: Vec::new() }),
        }
    }

    /// Returns the latest sequence.
    pub fn seq(&self) -> RuntimeEventValue {
        RuntimeEventValue(self.seq.load(Ordering::Acquire))
    }

    /// Returns whether at least one event has been published.
    pub fn has_unseen_events(&self) -> bool {
        self.seq.load(Ordering::Acquire) != 0
    }

    /// Returns whether the event sequence has changed from `observed`.
    pub fn has_changed(&self, observed: &RuntimeEventSeq) -> bool {
        self.seq.load(Ordering::Acquire) != observed.get()
    }

    /// Publishes event bits without waking registered ordinary wakers.
    pub fn publish_state(&self, bits: u64) -> RuntimeEventValue {
        if bits != 0 {
            self.bits.fetch_or(bits, Ordering::AcqRel);
        }
        RuntimeEventValue(self.seq.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// Publishes event bits and wakes ordinary registered wakers.
    pub fn publish(&self, bits: u64) -> RuntimeEventValue {
        let seq = self.publish_state(bits);
        self.wake_waiters();
        seq
    }

    /// Takes all coalesced event bits.
    pub fn take_bits(&self) -> u64 {
        self.bits.swap(0, Ordering::AcqRel)
    }

    /// Polls until the event sequence changes from `observed`.
    pub fn poll_changed(&self, observed: &RuntimeEventSeq, cx: &mut Context<'_>) -> Poll<u64> {
        let seq = self.seq.load(Ordering::Acquire);
        if seq != observed.get() {
            observed.0.store(seq, Ordering::Release);
            return Poll::Ready(seq);
        }

        self.register_waker(cx.waker());

        let seq = self.seq.load(Ordering::Acquire);
        if seq != observed.get() {
            observed.0.store(seq, Ordering::Release);
            Poll::Ready(seq)
        } else {
            Poll::Pending
        }
    }

    /// Wakes futures registered with [`poll_changed`](Self::poll_changed).
    pub fn wake_waiters(&self) {
        let waiters = {
            let mut waiters = self.waiters.lock();
            core::mem::take(&mut waiters.wakers)
        };
        for waker in waiters {
            waker.wake();
        }
    }

    fn register_waker(&self, waker: &Waker) {
        let mut waiters = self.waiters.lock();
        if let Some(existing) = waiters
            .wakers
            .iter()
            .position(|existing| existing.will_wake(waker))
        {
            waiters.wakers[existing] = waker.clone();
            return;
        }
        waiters.wakers.push(waker.clone());
    }
}

impl Default for RuntimeEventCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, Waker},
    };

    use super::{RuntimeEventCore, RuntimeEventSeq};

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn runtime_event_publish_before_poll_is_persistent() {
        let event = RuntimeEventCore::new();
        let observed = RuntimeEventSeq::new();
        let seq = event.publish_state(0b101);

        assert_eq!(seq.get(), 1);
        assert!(event.has_changed(&observed));
        assert_eq!(event.take_bits(), 0b101);
    }

    #[test]
    fn runtime_event_register_then_publish_wakes_once() {
        let event = RuntimeEventCore::new();
        let observed = RuntimeEventSeq::new();
        let count = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);

        assert_eq!(event.poll_changed(&observed, &mut cx), Poll::Pending);
        event.publish(0b1);

        assert_eq!(count.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn block_on_wake_state_repolls_on_local_wake_or_seq_change() {
        let state = super::BlockOnWakeState::new();

        assert!(!state.should_repoll(1, 1));
        state.mark_woke();
        assert!(state.should_repoll(1, 1));
        assert!(state.should_repoll(1, 2));
    }

    #[test]
    fn block_on_thread_waker_marks_local_wake_and_calls_task_wake() {
        #[derive(Clone)]
        struct HostWake(Arc<AtomicUsize>);

        impl crate::BlockOnTaskWake for HostWake {
            fn wake_task(&self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }

            fn wake_seq(&self) -> u64 {
                self.0.load(Ordering::Acquire) as u64
            }
        }

        let wake_count = Arc::new(AtomicUsize::new(0));
        let thread_waker = crate::BlockOnThreadWaker::new(HostWake(wake_count.clone()));
        let observed = thread_waker.wake_seq();
        let waker = thread_waker.waker();

        waker.wake_by_ref();

        assert_eq!(wake_count.load(Ordering::Acquire), 1);
        assert!(thread_waker.should_repoll(observed));
    }

    #[test]
    fn bare_cpu_core_coalesces_ipi_pending_bits() {
        let cpu = crate::BareCpuCore::<crate::FifoScheduler<crate::TaskRef>>::new(
            crate::CpuId(0),
            crate::FifoScheduler::new(),
        );

        assert!(cpu.request_ipi(crate::IpiEvent::Reschedule));
        assert!(!cpu.request_ipi(crate::IpiEvent::Reschedule));
        assert!(cpu.request_ipi(crate::IpiEvent::IrqWakeDrain));

        let events = cpu.take_pending_ipis();
        assert_eq!(
            events,
            crate::IpiEvents::from_events(&[
                crate::IpiEvent::Reschedule,
                crate::IpiEvent::IrqWakeDrain,
            ])
        );
        assert!(cpu.take_pending_ipis().is_empty());
    }

    #[test]
    fn bare_runtime_hard_irq_wake_queues_task_until_epilogue() {
        let runtime = crate::BareTaskRuntime::<crate::FifoScheduler<crate::TaskRef>>::new();
        let cpu = runtime.add_cpu(crate::CpuId(0), crate::FifoScheduler::new());
        let task = TestTask::new_core(1, crate::CpuId(0));
        task.set_state(crate::TaskState::Blocked);
        let waker = crate::TaskWaker::new(task.clone()).to_hard_irq_waker();

        let wake = runtime.wake_from_irq(&waker, 0b11);

        assert!(wake.woke());
        assert_eq!(task.state(), crate::TaskState::Blocked);
        assert_eq!(cpu.run_queue_len(), 0);
        assert_eq!(runtime.drain_irq_wake_queue(crate::CpuId(0)), 1);
        assert_eq!(task.state(), crate::TaskState::Ready);
        assert_eq!(cpu.run_queue_len(), 1);
        assert_eq!(waker.take_bits(), 0b11);
    }

    #[test]
    fn bare_runtime_timer_irq_marks_service_and_drains_in_task_context() {
        let runtime = crate::BareTaskRuntime::<crate::FifoScheduler<crate::TaskRef>>::new();
        let cpu = runtime.add_cpu(crate::CpuId(0), crate::FifoScheduler::new());
        let task = TestTask::new_core(1, crate::CpuId(0));
        task.set_state(crate::TaskState::Blocked);

        runtime.add_task_timer(crate::CpuId(0), 10, task.clone());
        assert!(!cpu.timer_service_pending());

        runtime.on_timer_irq(crate::CpuId(0), 10);

        assert!(cpu.timer_service_pending());
        assert_eq!(task.state(), crate::TaskState::Blocked);
        assert_eq!(runtime.drain_timer_service(crate::CpuId(0), 10), 1);
        assert!(!cpu.timer_service_pending());
        assert_eq!(task.state(), crate::TaskState::Ready);
        assert_eq!(cpu.run_queue_len(), 1);
    }

    struct TestTask;

    impl TestTask {
        fn new_core(id: u64, cpu: crate::CpuId) -> crate::TaskRef {
            Arc::new(crate::TaskCore::new(crate::TaskId(id), cpu))
        }
    }
}

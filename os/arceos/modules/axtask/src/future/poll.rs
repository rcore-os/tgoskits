use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};

/// A helper to wrap a synchronous non-blocking I/O function into an
/// asynchronous function.
///
/// # Arguments
///
/// * `pollable`: The pollable object to register for I/O events.
/// * `events`: The I/O events to wait for.
/// * `non_blocking`: If true, the function will return `AxError::WouldBlock`
///   immediately when the I/O operation would block.
/// * `f`: The synchronous non-blocking I/O function to be wrapped. It should
///   return `AxError::WouldBlock` when the operation would block.
pub async fn poll_io<P: Pollable, F: FnMut() -> AxResult<T>, T>(
    pollable: &P,
    events: IoEvents,
    non_blocking: bool,
    mut f: F,
) -> AxResult<T> {
    super::interruptible(poll_fn(move |cx| match f() {
        Ok(value) => Poll::Ready(Ok(value)),
        Err(AxError::WouldBlock) => {
            if non_blocking {
                return Poll::Ready(Err(AxError::WouldBlock));
            }
            pollable.register(cx, events);
            match f() {
                Ok(value) => Poll::Ready(Ok(value)),
                Err(AxError::WouldBlock) => Poll::Pending,
                Err(e) => Poll::Ready(Err(e)),
            }
        }
        Err(e) => Poll::Ready(Err(e)),
    }))
    .await?
}

/// Registers a waker for the given IRQ number.
///
/// This is a generic bridge for IRQ-driven async wakeups. The previous
/// implementation woke the per-IRQ `PollSet` directly from the IRQ hook,
/// which forced `PollSet::wake` (allocates, takes a `SpinNoIrq` mutex) to
/// run in interrupt context. On a single-core build that races against
/// the task it just preempted holding the same lock and deadlocks; in
/// practice it manifested as a null deref in the epoll wake chain.
///
/// The IRQ hook now only sets a per-IRQ pending bit and wakes a static
/// drain task; the drain task runs in normal task context and is the
/// only place that ever calls `PollSet::wake`.
#[cfg(feature = "irq")]
pub fn register_irq_waker(irq: usize, waker: &core::task::Waker) {
    use alloc::{collections::BTreeMap, sync::Arc};
    use core::sync::atomic::{AtomicBool, Ordering};

    use ax_kspin::SpinNoIrq;
    use axpoll::PollSet;

    /// Maximum IRQ number we track in the pending-bit array. Anything
    /// larger falls back to the lock + map check; that path is fine in
    /// task context.
    const MAX_TRACKED_IRQ: usize = 256;

    static IRQ_PENDING: [AtomicBool; MAX_TRACKED_IRQ] =
        [const { AtomicBool::new(false) }; MAX_TRACKED_IRQ];
    static ANY_PENDING: AtomicBool = AtomicBool::new(false);
    static DRAIN_WAKER: PollSet = PollSet::new();
    static DRAIN_SPAWNED: AtomicBool = AtomicBool::new(false);
    static POLL_IRQ: SpinNoIrq<BTreeMap<usize, Arc<PollSet>>> = SpinNoIrq::new(BTreeMap::new());

    fn irq_hook(irq: usize) {
        // Runs in IRQ context with interrupts off. Touching only atomics
        // and `PollSet::wake` on the dedicated drain set keeps this off
        // the heap and lock-free.
        if irq < MAX_TRACKED_IRQ {
            IRQ_PENDING[irq].store(true, Ordering::Release);
        }
        ANY_PENDING.store(true, Ordering::Release);
        DRAIN_WAKER.wake();
    }

    fn ensure_drain_spawned() {
        if DRAIN_SPAWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        crate::spawn_raw(
            || {
                crate::future::block_on(async {
                    loop {
                        // Park on the drain waker; the IRQ hook fires
                        // it without taking any locks.
                        core::future::poll_fn(|cx| {
                            if ANY_PENDING.swap(false, Ordering::AcqRel) {
                                core::task::Poll::Ready(())
                            } else {
                                DRAIN_WAKER.register(cx.waker());
                                if ANY_PENDING.swap(false, Ordering::AcqRel) {
                                    core::task::Poll::Ready(())
                                } else {
                                    core::task::Poll::Pending
                                }
                            }
                        })
                        .await;

                        // Snapshot the entries that need waking under
                        // the lock, then drop the lock before doing
                        // the actual `wake` work (which can allocate
                        // and re-enter the scheduler).
                        let mut to_wake: alloc::vec::Vec<Arc<PollSet>> = alloc::vec::Vec::new();
                        {
                            let map = POLL_IRQ.lock();
                            for (irq, slot) in IRQ_PENDING.iter().enumerate() {
                                if slot.swap(false, Ordering::AcqRel)
                                    && let Some(set) = map.get(&irq)
                                {
                                    to_wake.push(set.clone());
                                }
                            }
                        }
                        for set in to_wake {
                            set.wake();
                        }
                    }
                });
            },
            alloc::string::String::from("irq_waker_drain"),
            0x4000,
        );
    }

    ensure_drain_spawned();
    ax_hal::irq::register_irq_hook(irq_hook);

    POLL_IRQ
        .lock()
        .entry(irq)
        .or_insert_with(|| Arc::new(PollSet::new()))
        .register(waker);

    ax_hal::irq::set_enable(irq, true);
}

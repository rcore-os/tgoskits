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
            // Register the waker unconditionally before returning WouldBlock.
            // A non-blocking connect(2) returns EINPROGRESS; the caller then
            // uses epoll to wait for EPOLLOUT (connection complete).  If we
            // skip registration for non-blocking callers, the TCP stack has
            // no waker to call when the handshake finishes, so epoll never
            // receives the EPOLLOUT notification and the connection stalls.
            pollable.register(cx, events);
            if non_blocking {
                return Poll::Ready(Err(AxError::WouldBlock));
            }
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
/// This is a generic bridge for IRQ-driven async wakeups. Calling
/// `PollSet::wake` directly from an IRQ hook is unsafe: it takes a
/// `SpinNoIrq` mutex AND allocates (`Inner::new()` replacement when
/// there is an existing waiter), which can deadlock against the task
/// the IRQ preempted and triggers the slab from interrupt context.
///
/// The IRQ hook here does only what is safe in interrupt context:
/// flip a per-IRQ pending bit and `notify_one` a [`WaitQueue`].
/// `WaitQueue::notify_one` just pops from a `VecDeque` under a
/// `SpinNoIrq` (no allocation, deadlock-free because IRQs are
/// already disabled in the holding paths) and re-queues the drain
/// task. The drain task runs in normal task context and is the only
/// place that ever calls `PollSet::wake`.
#[cfg(feature = "irq")]
pub fn register_irq_waker(irq: usize, waker: &core::task::Waker) {
    use alloc::{collections::BTreeMap, sync::Arc};
    use core::{
        ptr::NonNull,
        sync::atomic::{AtomicBool, Ordering},
    };

    use ax_kspin::SpinNoIrq;
    use axpoll::PollSet;

    use crate::WaitQueue;

    /// Maximum IRQ number we track in the pending-bit array. The drain
    /// task scans IRQ_PENDING by index, so IRQs outside this range have
    /// no observable pending bit and the waker would never fire. We
    /// reject those at registration rather than silently dropping them.
    const MAX_TRACKED_IRQ: usize = 256;

    static IRQ_PENDING: [AtomicBool; MAX_TRACKED_IRQ] =
        [const { AtomicBool::new(false) }; MAX_TRACKED_IRQ];
    static IRQ_ACTION_INSTALLED: [AtomicBool; MAX_TRACKED_IRQ] =
        [const { AtomicBool::new(false) }; MAX_TRACKED_IRQ];
    static ANY_PENDING: AtomicBool = AtomicBool::new(false);
    static DRAIN_WQ: WaitQueue = WaitQueue::new();
    static DRAIN_SPAWNED: AtomicBool = AtomicBool::new(false);
    static POLL_IRQ: SpinNoIrq<BTreeMap<usize, Arc<PollSet>>> = SpinNoIrq::new(BTreeMap::new());

    unsafe fn irq_waker_handler(
        ctx: ax_hal::irq::IrqContext,
        _data: NonNull<()>,
    ) -> ax_hal::irq::IrqReturn {
        // Runs in IRQ context with interrupts off. Only atomics and a
        // `WaitQueue::notify_one` — no allocation, no PollSet/Inner
        // replacement.
        let irq = ctx.irq.0;
        if irq < MAX_TRACKED_IRQ {
            IRQ_PENDING[irq].store(true, Ordering::Release);
            ANY_PENDING.store(true, Ordering::Release);
            // `resched = false` because we cannot preempt from an IRQ
            // hook — let the scheduler run the drain task when the
            // current task next yields or reschedules.
            DRAIN_WQ.notify_one(false);
        }
        // IRQs >= MAX_TRACKED_IRQ are intentionally not tracked here.
        // register_irq_waker rejects those at registration, so reaching
        // this path means some other subsystem installed a handler on a
        // high IRQ — leave it alone instead of setting ANY_PENDING and
        // making the drain task spin.
        ax_hal::irq::IrqReturn::Handled
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
                loop {
                    // Block until at least one IRQ pending bit has
                    // been set. `wait_until` re-checks the condition
                    // under the wait-queue lock, so spurious wakeups
                    // do not slip through.
                    DRAIN_WQ.wait_until(|| ANY_PENDING.swap(false, Ordering::AcqRel));

                    // Snapshot the entries that need waking under the
                    // map lock, then drop the lock before invoking
                    // `wake` (which can allocate and re-enter the
                    // scheduler).
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
            },
            alloc::string::String::from("irq_waker_drain"),
            0x4000,
        );
    }

    if irq >= MAX_TRACKED_IRQ {
        warn!(
            "register_irq_waker: IRQ {irq} exceeds MAX_TRACKED_IRQ={MAX_TRACKED_IRQ}; ignoring \
             registration to avoid silently dropping wakeups"
        );
        return;
    }

    ensure_drain_spawned();

    if IRQ_ACTION_INSTALLED[irq]
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        ax_hal::irq::request_shared_irq(irq, irq_waker_handler, NonNull::dangling())
            .expect("axtask IRQ-waker bridge could not install shared IRQ action");
    }

    POLL_IRQ
        .lock()
        .entry(irq)
        .or_insert_with(|| Arc::new(PollSet::new()))
        .register(waker);

    ax_hal::irq::set_enable(irq, true);
}

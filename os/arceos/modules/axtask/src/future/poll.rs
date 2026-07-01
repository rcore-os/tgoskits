use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};

use crate::current;

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
    let curr = current();
    poll_fn(move |cx| {
        match f() {
            Ok(value) => return Poll::Ready(Ok(value)),
            Err(AxError::WouldBlock) => {}
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Register before the post-registration retry. A non-blocking
        // connect(2) returns EINPROGRESS; the caller then uses epoll to wait
        // for EPOLLOUT. If we skip registration for non-blocking callers, the
        // TCP stack has no waker to call when the handshake finishes.
        pollable.register(cx, events);

        match f() {
            Ok(value) => Poll::Ready(Ok(value)),
            Err(AxError::WouldBlock) if non_blocking => Poll::Ready(Err(AxError::WouldBlock)),
            Err(AxError::WouldBlock) => {
                if curr.poll_interrupt(cx).is_ready() {
                    Poll::Ready(Err(AxError::Interrupted))
                } else {
                    Poll::Pending
                }
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    })
    .await
}

/// Registers a waker for the given domain-scoped IRQ id.
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
pub fn register_irq_waker(irq: ax_hal::irq::IrqId, waker: &core::task::Waker) -> AxResult<()> {
    use alloc::{collections::BTreeMap, sync::Arc};
    use core::sync::atomic::{AtomicBool, Ordering};

    use ax_kspin::SpinNoIrq;
    use axpoll::PollSet;

    use crate::IrqNotify;

    static IRQ_NOTIFY: IrqNotify = IrqNotify::new();
    static DRAIN_SPAWNED: AtomicBool = AtomicBool::new(false);
    static IRQ_STATE: SpinNoIrq<BTreeMap<ax_hal::irq::IrqId, IrqPollState>> =
        SpinNoIrq::new(BTreeMap::new());

    struct IrqPollState {
        pending: bool,
        installed: bool,
        poll: Arc<PollSet>,
    }

    fn irq_waker_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
        // Runs in IRQ context with interrupts off. Only mark an already
        // registered slot and notify the drain task. The map entry is created
        // during task-context registration, so this path does not allocate.
        if let Some(state) = IRQ_STATE.lock().get_mut(&ctx.irq) {
            state.pending = true;
            IRQ_NOTIFY.notify_irq();
            ax_hal::irq::IrqReturn::Handled
        } else {
            ax_hal::irq::IrqReturn::Unhandled
        }
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
                    IRQ_NOTIFY.wait();

                    // Snapshot the entries that need waking under the
                    // map lock, then drop the lock before invoking
                    // `wake` (which can allocate and re-enter the
                    // scheduler).
                    let mut to_wake: alloc::vec::Vec<Arc<PollSet>> = alloc::vec::Vec::new();
                    {
                        let mut map = IRQ_STATE.lock();
                        for state in map.values_mut() {
                            if state.pending {
                                state.pending = false;
                                to_wake.push(state.poll.clone());
                            }
                        }
                    }
                    for set in to_wake {
                        unsafe { set.wake(axpoll::IoEvents::all()) };
                    }
                }
            },
            alloc::string::String::from("irq_waker_drain"),
            0x4000,
        );
    }

    ensure_drain_spawned();

    let (poll, should_install) = {
        let mut map = IRQ_STATE.lock();
        let state = map.entry(irq).or_insert_with(|| IrqPollState {
            pending: false,
            installed: false,
            poll: Arc::new(PollSet::new()),
        });
        if state.installed {
            (state.poll.clone(), false)
        } else {
            state.installed = true;
            (state.poll.clone(), true)
        }
    };
    unsafe { poll.register(waker, axpoll::IoEvents::all()) };

    if should_install {
        ax_hal::irq::request_shared_irq(irq, irq_waker_handler)
            .map_err(|_| AxError::Unsupported)?;
    }

    ax_hal::irq::set_enable(irq, true).map_err(|_| AxError::Unsupported)
}

/// Registers a waker for a temporary legacy numeric IRQ.
pub fn register_legacy_irq_waker(irq: usize, waker: &core::task::Waker) -> AxResult<()> {
    let irq = ax_hal::irq::try_legacy_irq(irq).map_err(|_| AxError::InvalidInput)?;
    register_irq_waker(irq, waker)
}

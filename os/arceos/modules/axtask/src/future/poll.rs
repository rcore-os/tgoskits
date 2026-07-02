use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet, Pollable};

use crate::{HardIrqSignal, current};

static IRQ_NOTIFY: HardIrqSignal = HardIrqSignal::new();
static DRAIN_SPAWNED: AtomicBool = AtomicBool::new(false);
static IRQ_STATE: SpinNoIrq<BTreeMap<ax_hal::irq::IrqId, Arc<IrqPollState>>> =
    SpinNoIrq::new(BTreeMap::new());

struct IrqPollState {
    pending: AtomicBool,
    installed: AtomicBool,
    poll: Arc<PollSet>,
}

impl IrqPollState {
    fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            installed: AtomicBool::new(false),
            poll: Arc::new(PollSet::new()),
        }
    }

    fn handle_irq(&self) -> ax_hal::irq::IrqReturn {
        self.pending.store(true, Ordering::Release);
        IRQ_NOTIFY.notify_irq();
        ax_hal::irq::IrqReturn::Handled
    }

    fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    fn mark_installing(&self) -> bool {
        !self.installed.swap(true, Ordering::AcqRel)
    }

    fn clear_installing(&self) {
        self.installed.store(false, Ordering::Release);
    }
}

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
/// The IRQ hook here does only what is safe in interrupt context: flip an
/// already-allocated per-IRQ pending bit and poke a [`HardIrqSignal`]. The drain
/// task runs in normal task context and is the only place that locks the
/// registry or calls `PollSet::wake`.
pub fn register_irq_waker(irq: ax_hal::irq::IrqId, waker: &core::task::Waker) -> AxResult<()> {
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
                    let mut to_wake: Vec<Arc<PollSet>> = Vec::new();
                    {
                        let map = IRQ_STATE.lock();
                        for state in map.values() {
                            if state.take_pending() {
                                to_wake.push(state.poll.clone());
                            }
                        }
                    }
                    for set in to_wake {
                        unsafe { set.wake(axpoll::IoEvents::all()) };
                    }
                }
            },
            String::from("irq_waker_drain"),
            0x4000,
        );
    }

    ensure_drain_spawned();

    let state = {
        let mut map = IRQ_STATE.lock();
        map.entry(irq)
            .or_insert_with(|| Arc::new(IrqPollState::new()))
            .clone()
    };
    unsafe { state.poll.register(waker, axpoll::IoEvents::all()) };

    if state.mark_installing() {
        let handler_state = state.clone();
        if ax_hal::irq::request_shared_irq(irq, move |_| handler_state.handle_irq()).is_err() {
            state.clear_installing();
            return Err(AxError::Unsupported);
        }
    }

    ax_hal::irq::set_enable(irq, true).map_err(|_| AxError::Unsupported)
}

/// Registers a waker for a temporary legacy numeric IRQ.
pub fn register_legacy_irq_waker(irq: usize, waker: &core::task::Waker) -> AxResult<()> {
    let irq = ax_hal::irq::try_legacy_irq(irq).map_err(|_| AxError::InvalidInput)?;
    register_irq_waker(irq, waker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_poll_handler_does_not_need_registry_lock() {
        let state = Arc::new(IrqPollState::new());
        let _registry_guard = IRQ_STATE.lock();

        assert_eq!(state.handle_irq(), ax_hal::irq::IrqReturn::Handled);
        assert!(state.take_pending());
    }
}

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

#[cfg(feature = "irq")]
/// Registers a waker for the given IRQ number.
///
/// This is a generic transition helper for IRQ-driven async wakeups. New
/// console/TTY and Starry NIC code should prefer platform-specific IRQ
/// handlers over this bridge.
pub fn register_irq_waker(irq: usize, waker: &core::task::Waker) {
    use alloc::collections::BTreeMap;

    use ax_kspin::SpinNoIrq;
    use axpoll::PollSet;

    static POLL_IRQ: SpinNoIrq<BTreeMap<usize, PollSet>> = SpinNoIrq::new(BTreeMap::new());

    fn irq_hook(irq: usize) {
        if let Some(s) = POLL_IRQ.lock().get(&irq) {
            s.wake();
        }
    }
    ax_hal::irq::register_irq_hook(irq_hook);

    POLL_IRQ.lock().entry(irq).or_default().register(waker);

    ax_hal::irq::set_enable(irq, true);
}

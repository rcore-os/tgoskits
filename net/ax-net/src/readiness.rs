//! Task-neutral readiness futures for socket operations.
//!
//! This module owns only the check/register/recheck handshake.  The consuming
//! OS chooses how the future is driven, timed out, or interrupted; ax-net never
//! parks a scheduler thread or interprets a userspace signal.

use core::{future::poll_fn, task::Poll};

use axpoll::{ExclusiveConsumer, IoEvents, PollRegistrar, Pollable};

use crate::{NetError, NetResult};

/// Repeats one non-blocking operation after race-free readiness registration.
///
/// Dropping the future clears its registrar, so an OS-level timeout, signal, or
/// task cancellation cannot leave a stale socket waiter behind.
pub async fn poll_socket_io<P, F, T>(
    pollable: &P,
    events: IoEvents,
    nonblocking: bool,
    mut operation: F,
) -> NetResult<T>
where
    P: Pollable + ?Sized,
    F: FnMut() -> NetResult<T>,
{
    let mut registrar = None::<PollRegistrar<ExclusiveConsumer>>;
    poll_fn(move |context| {
        if let Some(registrar) = registrar.as_mut() {
            registrar.reset(context.waker());
        }
        match operation() {
            Ok(value) => return Poll::Ready(Ok(value)),
            Err(NetError::WouldBlock) => {}
            Err(error) => return Poll::Ready(Err(error)),
        }

        let registrar = registrar.get_or_insert_with(|| PollRegistrar::new(context.waker()));
        unsafe { pollable.register_exclusive(registrar, events) };
        match operation() {
            Ok(value) => {
                registrar.clear();
                Poll::Ready(Ok(value))
            }
            Err(NetError::WouldBlock) if nonblocking => {
                registrar.clear();
                Poll::Ready(Err(NetError::WouldBlock))
            }
            Err(NetError::WouldBlock) => Poll::Pending,
            Err(error) => {
                registrar.clear();
                Poll::Ready(Err(error))
            }
        }
    })
    .await
}

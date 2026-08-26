//! File and poll boundary adapters for epoll instances.

use alloc::borrow::Cow;

use axpoll::{ExclusiveRegistrationSink, IoEvents, Pollable, SharedRegistrationSink};

use super::{FileLike, epoll::Epoll};

impl FileLike for Epoll {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[eventpoll]".into()
    }
}

impl Pollable for Epoll {
    fn poll(&self) -> IoEvents {
        if self.inner.has_ready_events() {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            unsafe { self.inner.register_shared_poll_waiter(sink) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if events.contains(IoEvents::IN) {
            unsafe { self.inner.register_exclusive_poll_waiter(sink) };
        }
    }
}

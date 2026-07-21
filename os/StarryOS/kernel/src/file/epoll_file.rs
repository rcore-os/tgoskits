//! File and poll boundary adapters for epoll instances.

use alloc::borrow::Cow;
use core::task::Context;

use axpoll::{IoEvents, Pollable};

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

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.inner.register_poll_waiter(context);
        }
    }
}

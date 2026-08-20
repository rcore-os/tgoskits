mod epoll;
mod poll;
mod select;

use alloc::{sync::Arc, vec::Vec};

use axpoll::{IoEvents, Pollable, SharedRegistrationSink};

pub use self::{epoll::*, poll::*, select::*};
use crate::file::FileLike;

struct FdPollSet(pub Vec<(Arc<dyn FileLike>, IoEvents)>);
impl Pollable for FdPollSet {
    fn poll(&self) -> IoEvents {
        unreachable!()
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, _events: IoEvents) {
        for (file, events) in &self.0 {
            unsafe { file.register_shared(sink, *events) };
        }
    }
}

#[cfg(test)]
pub(crate) use self::epoll::epoll_validation_rules_hold_for_test;
#[cfg(test)]
pub(crate) use self::poll::poll_nfds_validation_rules_hold_for_test;
#[cfg(test)]
pub(crate) use self::select::select_fd_set_and_validation_rules_hold_for_test;

//! Deterministic concurrency hooks for epoll kernel tests.

use alloc::{borrow::Cow, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::Context,
};

use ax_errno::AxError;
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, Pollable};

use super::{FileLike, epoll::Epoll};

static EPOLL_ADD_TEST_BARRIER_ENABLED: AtomicBool = AtomicBool::new(false);
static EPOLL_ADD_TEST_BARRIER_ARRIVALS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn epoll_add_test_barrier() {
    if !EPOLL_ADD_TEST_BARRIER_ENABLED.load(Ordering::Acquire) {
        return;
    }

    EPOLL_ADD_TEST_BARRIER_ARRIVALS.fetch_add(1, Ordering::AcqRel);
    while EPOLL_ADD_TEST_BARRIER_ARRIVALS.load(Ordering::Acquire) < 2 {
        ax_task::yield_now();
    }
}

pub(crate) fn concurrent_reverse_add_is_serialized_for_test() -> bool {
    let left = Arc::new(Epoll::new());
    let right = Arc::new(Epoll::new());
    let results = Arc::new(SpinNoIrq::new([None, None]));

    EPOLL_ADD_TEST_BARRIER_ARRIVALS.store(0, Ordering::Release);
    EPOLL_ADD_TEST_BARRIER_ENABLED.store(true, Ordering::Release);

    let left_task = {
        let left = Arc::clone(&left);
        let right = Arc::clone(&right);
        let results = Arc::clone(&results);
        ax_task::spawn(move || {
            results.lock()[0] = left.add_nested_for_test(1, right).err();
        })
    };
    let right_task = {
        let left = Arc::clone(&left);
        let right = Arc::clone(&right);
        let results = Arc::clone(&results);
        ax_task::spawn(move || {
            results.lock()[1] = right.add_nested_for_test(2, left).err();
        })
    };

    left_task.join();
    right_task.join();
    EPOLL_ADD_TEST_BARRIER_ENABLED.store(false, Ordering::Release);

    let results = results.lock();
    matches!(
        results.as_slice(),
        [None, Some(AxError::FilesystemLoop)] | [Some(AxError::FilesystemLoop), None]
    )
}

struct ReadyDuringRegisterFile {
    ready: AtomicBool,
}

impl ReadyDuringRegisterFile {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(true),
        }
    }

    fn clear_ready(&self) {
        self.ready.store(false, Ordering::Release);
    }
}

impl FileLike for ReadyDuringRegisterFile {
    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-rearm-race]".into()
    }
}

impl Pollable for ReadyDuringRegisterFile {
    fn poll(&self) -> IoEvents {
        if self.ready.load(Ordering::Acquire) {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    fn register(&self, _context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            // Model readiness becoming visible after the old wake was consumed
            // but before the replacement waker can observe a new transition.
            self.ready.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn epoll_requeues_readiness_observed_during_rearm_for_test() -> bool {
    let epoll = Epoll::new();
    let file = Arc::new(ReadyDuringRegisterFile::new());
    let file_like: Arc<dyn FileLike> = file.clone();

    if epoll.add_file_for_test(17, file_like).is_err() {
        return false;
    }
    file.clear_ready();

    if epoll.poll_events_with(1, |_, _| Ok(())).err() != Some(AxError::WouldBlock) {
        return false;
    }

    let mut observed = None;
    epoll
        .poll_events_with(1, |_, event| {
            observed = Some(event);
            Ok(())
        })
        .is_ok_and(|count| {
            count == 1
                && observed.is_some_and(|event| {
                    event.data == 17
                        && IoEvents::from_bits_retain(event.events).contains(IoEvents::IN)
                })
        })
}

//! Deterministic concurrency hooks for epoll kernel tests.

use alloc::{borrow::Cow, sync::Arc, task::Wake};
use core::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Waker},
};

use ax_errno::AxError;
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet, Pollable};

use super::{
    FileLike,
    epoll::{Epoll, EpollFlags},
};

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

struct ReadyFile {
    ready: AtomicBool,
    poll_waiters: PollSet,
}

impl ReadyFile {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: AtomicBool::new(false),
            poll_waiters: PollSet::new(),
        })
    }

    fn make_ready(&self) {
        self.ready.store(true, Ordering::Release);
        unsafe { self.poll_waiters.wake(IoEvents::IN) };
    }
}

impl FileLike for ReadyFile {
    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-ready-file]".into()
    }
}

impl Pollable for ReadyFile {
    fn poll(&self) -> IoEvents {
        if self.ready.load(Ordering::Acquire) {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        unsafe { self.poll_waiters.register(context.waker(), events) };
    }
}

struct EpollWaiter {
    epoll: Arc<Epoll>,
    result_index: usize,
    results: Arc<SpinNoIrq<[Option<u64>; 2]>>,
}

impl EpollWaiter {
    fn collect_one(&self) {
        let mut user_data = None;
        let result = self.epoll.poll_events_with(1, |_index, event| {
            user_data = Some(event.data);
            Ok(())
        });
        if matches!(result, Ok(1)) {
            self.results.lock()[self.result_index] = user_data;
        }
    }
}

impl Wake for EpollWaiter {
    fn wake(self: Arc<Self>) {
        self.collect_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.collect_one();
    }
}

pub(crate) fn level_aliases_rotate_in_linux_callback_order_for_test() -> bool {
    let epoll = Arc::new(Epoll::new());
    let target = ReadyFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();
    let results = Arc::new(SpinNoIrq::new([None, None]));

    epoll
        .add_file_for_test(1, target_file.clone(), 0x11, EpollFlags::empty())
        .expect("first test interest must be added");
    epoll
        .add_file_for_test(2, target_file, 0x22, EpollFlags::empty())
        .expect("second test interest must be added");

    for result_index in 0..2 {
        let waiter = Arc::new(EpollWaiter {
            epoll: epoll.clone(),
            result_index,
            results: results.clone(),
        });
        let waker = Waker::from(waiter);
        let mut context = Context::from_waker(&waker);
        epoll.register(&mut context, IoEvents::IN);
    }

    target.make_ready();
    results.lock().as_slice() == [Some(0x22), Some(0x11)]
}

pub(crate) fn edge_readiness_requires_a_new_notification_for_test() -> bool {
    let epoll = Epoll::new();
    let target = ReadyFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file, 0x33, EpollFlags::EDGE_TRIGGER)
        .expect("edge-triggered test interest must be added");

    target.make_ready();
    let first = collect_one_event(&epoll);
    let without_new_notification = collect_one_event(&epoll);
    target.make_ready();
    let after_new_notification = collect_one_event(&epoll);

    first == Ok((1, Some(0x33)))
        && without_new_notification == Err(AxError::WouldBlock)
        && after_new_notification == Ok((1, Some(0x33)))
}

fn collect_one_event(epoll: &Epoll) -> Result<(usize, Option<u64>), AxError> {
    let mut user_data = None;
    let count = epoll.poll_events_with(1, |_index, event| {
        user_data = Some(event.data);
        Ok(())
    })?;
    Ok((count, user_data))
}

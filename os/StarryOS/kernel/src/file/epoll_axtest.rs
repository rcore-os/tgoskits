//! Deterministic concurrency hooks for epoll kernel tests.

use alloc::sync::Arc;
#[cfg(all(test, not(axtest)))]
use alloc::{borrow::Cow, task::Wake};
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(all(test, not(axtest)))]
use core::task::{Context, Waker};

#[cfg(all(test, axtest))]
use core::sync::atomic::AtomicUsize;

#[cfg(all(test, not(axtest)))]
use axpoll::{IoEvents, PollSet, Pollable};

use super::epoll::Epoll;
#[cfg(all(test, not(axtest)))]
use super::{FileLike, epoll::EpollFlags};
use crate::{StarryError, sync::IrqMutex};

#[cfg(all(test, axtest))]
static EPOLL_ADD_TEST_BARRIER_ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(all(test, axtest))]
static EPOLL_ADD_TEST_BARRIER_ARRIVALS: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, axtest))]
pub(super) fn epoll_add_test_barrier() {
    if !EPOLL_ADD_TEST_BARRIER_ENABLED.load(Ordering::Acquire) {
        return;
    }

    EPOLL_ADD_TEST_BARRIER_ARRIVALS.fetch_add(1, Ordering::AcqRel);
    while EPOLL_ADD_TEST_BARRIER_ARRIVALS.load(Ordering::Acquire) < 2 {
        ax_task::yield_now();
    }
}

#[cfg(all(test, axtest))]
fn concurrent_reverse_add_is_serialized_for_test() -> bool {
    let left = Arc::new(Epoll::new());
    let right = Arc::new(Epoll::new());
    let results = Arc::new(IrqMutex::new([None, None]));

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
        [None, Some(StarryError::FilesystemLoop)] | [Some(StarryError::FilesystemLoop), None]
    )
}

#[cfg(all(test, not(axtest)))]
struct ReadyFile {
    ready: AtomicBool,
    poll_waiters: PollSet,
}

#[cfg(all(test, not(axtest)))]
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

#[cfg(all(test, not(axtest)))]
impl FileLike for ReadyFile {
    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-ready-file]".into()
    }
}

#[cfg(all(test, not(axtest)))]
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

#[cfg(all(test, not(axtest)))]
struct CallbackBoundaryFile {
    ready: AtomicBool,
    waking: AtomicBool,
    callback_reentered_file: AtomicBool,
    poll_waiters: PollSet,
}

#[cfg(all(test, not(axtest)))]
impl CallbackBoundaryFile {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: AtomicBool::new(false),
            waking: AtomicBool::new(false),
            callback_reentered_file: AtomicBool::new(false),
            poll_waiters: PollSet::new(),
        })
    }

    fn make_ready(&self) {
        self.ready.store(true, Ordering::Release);
        self.waking.store(true, Ordering::Release);
        unsafe { self.poll_waiters.wake(IoEvents::IN) };
        self.waking.store(false, Ordering::Release);
    }

    fn callback_reentered_file(&self) -> bool {
        self.callback_reentered_file.load(Ordering::Acquire)
    }

    fn record_callback_reentry(&self) {
        if self.waking.load(Ordering::Acquire) {
            self.callback_reentered_file.store(true, Ordering::Release);
        }
    }
}

#[cfg(all(test, not(axtest)))]
impl FileLike for CallbackBoundaryFile {
    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-callback-boundary-file]".into()
    }
}

#[cfg(all(test, not(axtest)))]
impl Pollable for CallbackBoundaryFile {
    fn poll(&self) -> IoEvents {
        self.record_callback_reentry();
        if self.ready.load(Ordering::Acquire) {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.record_callback_reentry();
        unsafe { self.poll_waiters.register(context.waker(), events) };
    }
}

#[cfg(all(test, not(axtest)))]
struct EpollWaiter {
    epoll: Arc<Epoll>,
    result_index: usize,
    results: Arc<IrqMutex<[Option<u64>; 2]>>,
}

#[cfg(all(test, not(axtest)))]
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

#[cfg(all(test, not(axtest)))]
impl Wake for EpollWaiter {
    fn wake(self: Arc<Self>) {
        self.collect_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.collect_one();
    }
}

#[cfg(all(test, not(axtest)))]
fn level_aliases_rotate_in_linux_callback_order_for_test() -> bool {
    let epoll = Arc::new(Epoll::new());
    let target = ReadyFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();
    let results = Arc::new(IrqMutex::new([None, None]));

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

#[cfg(all(test, not(axtest)))]
fn edge_readiness_requires_a_new_notification_for_test() -> bool {
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

    matches!(first, Ok((1, Some(0x33))))
        && matches!(without_new_notification, Err(StarryError::WouldBlock))
        && matches!(after_new_notification, Ok((1, Some(0x33))))
}

#[cfg(all(test, not(axtest)))]
fn edge_callback_does_not_reenter_target_for_test() -> bool {
    let epoll = Epoll::new();
    let target = CallbackBoundaryFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file, 0x44, EpollFlags::EDGE_TRIGGER)
        .expect("edge-triggered test interest must be added");

    target.make_ready();

    !target.callback_reentered_file()
}

#[cfg(all(test, not(axtest)))]
fn level_callback_does_not_reenter_target_for_test() -> bool {
    let epoll = Epoll::new();
    let target = CallbackBoundaryFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file, 0x45, EpollFlags::empty())
        .expect("level-triggered test interest must be added");

    target.make_ready();

    !target.callback_reentered_file()
}

#[cfg(all(test, not(axtest)))]
fn collect_one_event(epoll: &Epoll) -> Result<(usize, Option<u64>), StarryError> {
    let mut user_data = None;
    let count = epoll.poll_events_with(1, |_index, event| {
        user_data = Some(event.data);
        Ok(())
    })?;
    Ok((count, user_data))
}

#[cfg(all(test, not(axtest)))]
struct ReadyDuringRegisterFile {
    ready: AtomicBool,
}

#[cfg(all(test, not(axtest)))]
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

#[cfg(all(test, not(axtest)))]
impl FileLike for ReadyDuringRegisterFile {
    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-rearm-race]".into()
    }
}

#[cfg(all(test, not(axtest)))]
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

#[cfg(all(test, not(axtest)))]
fn epoll_requeues_readiness_observed_during_rearm_for_test() -> bool {
    let epoll = Epoll::new();
    let file = Arc::new(ReadyDuringRegisterFile::new());
    let file_like: Arc<dyn FileLike> = file.clone();

    if epoll
        .add_file_for_test(17, file_like, 17, EpollFlags::empty())
        .is_err()
    {
        return false;
    }
    file.clear_ready();

    if !matches!(
        epoll.poll_events_with(1, |_, _| Ok(())).err(),
        Some(StarryError::WouldBlock)
    ) {
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

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn concurrent_reverse_add_is_serialized() {
        assert!(super::concurrent_reverse_add_is_serialized_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn level_aliases_rotate_in_linux_callback_order() {
        assert!(super::level_aliases_rotate_in_linux_callback_order_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn edge_readiness_requires_a_new_notification() {
        assert!(super::edge_readiness_requires_a_new_notification_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn edge_callback_does_not_reenter_target() {
        assert!(super::edge_callback_does_not_reenter_target_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn level_callback_does_not_reenter_target() {
        assert!(super::level_callback_does_not_reenter_target_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn requeues_readiness_observed_during_rearm() {
        assert!(super::epoll_requeues_readiness_observed_during_rearm_for_test());
    }
}

//! Deterministic concurrency hooks for epoll kernel tests.

use alloc::{sync::Arc, task::Wake};
#[cfg(all(test, not(axtest)))]
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::Waker;

use axpoll::{ExclusiveConsumer, IoEvents, PollRegistrar, Pollable};
#[cfg(all(test, not(axtest)))]
use axpoll::{
    ExclusiveRegistrationSink, PollRegistration, PollSource, RegistrationMode,
    SharedRegistrationSink,
};
#[cfg(all(test, not(axtest)))]
use axpoll_set::PollSet;

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
        crate::task::yield_now();
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
        crate::task::kernel_thread_builder("epoll-axtest-left".into())
            .spawn(move || {
                results.lock()[0] = left.add_nested_for_test(1, right).err();
            })
            .expect("failed to spawn kernel thread")
    };
    let right_task = {
        let left = Arc::clone(&left);
        let right = Arc::clone(&right);
        let results = Arc::clone(&results);
        crate::task::kernel_thread_builder("epoll-axtest-right".into())
            .spawn(move || {
                results.lock()[1] = right.add_nested_for_test(2, left).err();
            })
            .expect("failed to spawn kernel thread")
    };

    left_task.join().expect("failed to join kernel thread");
    right_task.join().expect("failed to join kernel thread");
    EPOLL_ADD_TEST_BARRIER_ENABLED.store(false, Ordering::Release);

    let results = results.lock();
    matches!(
        results.as_slice(),
        [None, Some(StarryError::FilesystemLoop)] | [Some(StarryError::FilesystemLoop), None]
    )
}

#[cfg(all(test, axtest))]
struct DeferredWakeWaiter {
    woken: AtomicBool,
}

#[cfg(all(test, axtest))]
impl Wake for DeferredWakeWaiter {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }
}

#[cfg(all(test, axtest))]
fn epoll_notify_worker_flushes_deferred_wake_for_test() -> bool {
    let epoll = Epoll::new();
    let waiter = Arc::new(DeferredWakeWaiter {
        woken: AtomicBool::new(false),
    });
    let waker = Waker::from(Arc::clone(&waiter));
    let mut registrar = PollRegistrar::<ExclusiveConsumer>::new(&waker);
    unsafe { epoll.register_exclusive(&mut registrar, IoEvents::IN) };

    epoll.defer_ready_waiters_for_test(1);
    for _ in 0..1024 {
        if waiter.woken.load(Ordering::Acquire) {
            return true;
        }
        crate::task::yield_now();
    }
    false
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
    fn validate_write_access(&self) -> crate::StarryResult {
        Err(StarryError::InvalidInput)
    }

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

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        unsafe { sink.register_shared(&self.poll_waiters, events) };
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { sink.register_exclusive(&self.poll_waiters, events) };
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
    fn validate_write_access(&self) -> crate::StarryResult {
        Err(StarryError::InvalidInput)
    }

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

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        self.record_callback_reentry();
        unsafe { sink.register_shared(&self.poll_waiters, events) };
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
struct TestPollRegistration;

#[cfg(all(test, not(axtest)))]
impl PollRegistration for TestPollRegistration {
    fn was_notified(&self) -> bool {
        false
    }
}

#[cfg(all(test, not(axtest)))]
struct WakeDuringRegisterSource {
    registrations: AtomicUsize,
    wakers: IrqMutex<Vec<Waker>>,
}

#[cfg(all(test, not(axtest)))]
impl PollSource for WakeDuringRegisterSource {
    unsafe fn register(
        &self,
        waker: &Waker,
        _interests: IoEvents,
        _mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        let previous = {
            let mut wakers = self.wakers.lock();
            let previous = wakers.last().cloned();
            wakers.push(waker.clone());
            previous
        };
        if self.registrations.fetch_add(1, Ordering::AcqRel) == 1
            && let Some(previous) = previous
        {
            previous.wake_by_ref();
        }
        Some(Box::new(TestPollRegistration))
    }
}

#[cfg(all(test, not(axtest)))]
struct WakeDuringRegisterFile {
    registering: AtomicBool,
    callback_reentered_file: AtomicBool,
    source: WakeDuringRegisterSource,
}

#[cfg(all(test, not(axtest)))]
impl WakeDuringRegisterFile {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            registering: AtomicBool::new(false),
            callback_reentered_file: AtomicBool::new(false),
            source: WakeDuringRegisterSource {
                registrations: AtomicUsize::new(0),
                wakers: IrqMutex::new(Vec::new()),
            },
        })
    }

    fn record_callback_reentry(&self) {
        if self.registering.load(Ordering::Acquire) {
            self.callback_reentered_file.store(true, Ordering::Release);
        }
    }
}

#[cfg(all(test, not(axtest)))]
impl FileLike for WakeDuringRegisterFile {
    fn validate_write_access(&self) -> crate::StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-wake-during-register]".into()
    }
}

#[cfg(all(test, not(axtest)))]
impl Pollable for WakeDuringRegisterFile {
    fn poll(&self) -> IoEvents {
        self.record_callback_reentry();
        IoEvents::empty()
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        self.registering.store(true, Ordering::Release);
        unsafe { sink.register_shared(&self.source, events) };
        self.registering.store(false, Ordering::Release);
    }
}

#[cfg(all(test, not(axtest)))]
fn wake_during_registration_is_deferred_for_test() -> bool {
    let epoll = Arc::new(Epoll::new());
    let target = WakeDuringRegisterFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();

    if epoll
        .add_file_for_test(1, target_file, 0x46, EpollFlags::empty())
        .is_err()
    {
        return false;
    }

    let waiter = Arc::new(EpollWaiter {
        epoll: epoll.clone(),
        result_index: 0,
        results: Arc::new(IrqMutex::new([None, None])),
    });
    let waker = Waker::from(waiter);
    let mut registrar = PollRegistrar::<ExclusiveConsumer>::new(&waker);
    unsafe { epoll.register_exclusive(&mut registrar, IoEvents::IN) };

    epoll.register_waiter_wakers().is_ok()
        && !target.callback_reentered_file.load(Ordering::Acquire)
}

#[cfg(all(test, not(axtest)))]
fn level_aliases_are_both_delivered_for_test() -> bool {
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

    let mut registrations = Vec::new();
    for result_index in 0..2 {
        let waiter = Arc::new(EpollWaiter {
            epoll: epoll.clone(),
            result_index,
            results: results.clone(),
        });
        let waker = Waker::from(waiter);
        let mut registrar = PollRegistrar::<ExclusiveConsumer>::new(&waker);
        unsafe { epoll.register_exclusive(&mut registrar, IoEvents::IN) };
        registrations.push(registrar);
    }

    target.make_ready();
    epoll.flush_ready_waiters_for_test();
    matches!(
        results.lock().as_slice(),
        [Some(0x11), Some(0x22)] | [Some(0x22), Some(0x11)]
    )
}

#[cfg(all(test, not(axtest)))]
fn exclusive_aliases_publish_only_one_interest_for_test() -> bool {
    let epoll = Epoll::new();
    let target = ReadyFile::new();
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file.clone(), 0x51, EpollFlags::EXCLUSIVE)
        .expect("first exclusive test interest must be added");
    epoll
        .add_file_for_test(2, target_file, 0x52, EpollFlags::EXCLUSIVE)
        .expect("second exclusive test interest must be added");

    target.make_ready();
    let mut user_data = Vec::new();
    epoll
        .poll_events_with(2, |_index, event| {
            user_data.push(event.data);
            Ok(())
        })
        .is_ok_and(|count| {
            count == 1 && matches!(user_data.as_slice(), [0x51] | [0x52])
        })
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
    fn validate_write_access(&self) -> crate::StarryResult {
        Err(StarryError::InvalidInput)
    }

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

    unsafe fn register_shared(&self, _sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
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

#[cfg(all(test, not(axtest)))]
struct CountedLease {
    lease: Box<dyn PollRegistration>,
    live: Arc<AtomicUsize>,
}

#[cfg(all(test, not(axtest)))]
impl PollRegistration for CountedLease {
    fn was_notified(&self) -> bool {
        self.lease.was_notified()
    }
}

#[cfg(all(test, not(axtest)))]
impl Drop for CountedLease {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(all(test, not(axtest)))]
struct CountedSource {
    waiters: PollSet,
    registered: AtomicUsize,
    live: Arc<AtomicUsize>,
}

#[cfg(all(test, not(axtest)))]
impl CountedSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            waiters: PollSet::new(),
            registered: AtomicUsize::new(0),
            live: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn registered(&self) -> usize {
        self.registered.load(Ordering::Acquire)
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }
}

#[cfg(all(test, not(axtest)))]
impl PollSource for CountedSource {
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        let lease = unsafe { self.waiters.register(waker, interests, mode) }?;
        self.registered.fetch_add(1, Ordering::AcqRel);
        self.live.fetch_add(1, Ordering::AcqRel);
        Some(Box::new(CountedLease {
            lease,
            live: Arc::clone(&self.live),
        }))
    }
}

// The source is shared rather than owned so a test can keep it alive after the
// file is gone, like a FIFO buffer that outlives one of its open files.
#[cfg(all(test, not(axtest)))]
struct CountedFile {
    ready: AtomicBool,
    source: Arc<CountedSource>,
}

#[cfg(all(test, not(axtest)))]
impl CountedFile {
    fn new(ready: bool, source: &Arc<CountedSource>) -> Arc<Self> {
        Arc::new(Self {
            ready: AtomicBool::new(ready),
            source: Arc::clone(source),
        })
    }

    fn make_ready(&self) {
        self.ready.store(true, Ordering::Release);
        unsafe { self.source.waiters.wake(IoEvents::IN) };
    }
}

#[cfg(all(test, not(axtest)))]
impl FileLike for CountedFile {
    fn validate_write_access(&self) -> crate::StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        "axtest:[epoll-counted-file]".into()
    }
}

#[cfg(all(test, not(axtest)))]
impl Pollable for CountedFile {
    fn poll(&self) -> IoEvents {
        if self.ready.load(Ordering::Acquire) {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        unsafe { sink.register_shared(self.source.as_ref(), events) };
    }
}

#[cfg(all(test, not(axtest)))]
fn armed_lease_survives_repeated_waits_for_test() -> bool {
    let epoll = Epoll::new();
    let source = CountedSource::new();
    let target = CountedFile::new(false, &source);
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file, 0x61, EpollFlags::empty())
        .expect("level-triggered test interest must be added");
    for _ in 0..8 {
        if epoll.register_waiter_wakers().is_err() {
            return false;
        }
    }
    let registered_once = source.registered() == 1 && source.live() == 1;

    target.make_ready();
    registered_once && matches!(collect_one_event(&epoll), Ok((1, Some(0x61))))
}

#[cfg(all(test, not(axtest)))]
fn notified_lease_is_registered_again_for_test() -> bool {
    let epoll = Epoll::new();
    let source = CountedSource::new();
    let target = CountedFile::new(false, &source);
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file, 0x62, EpollFlags::EDGE_TRIGGER)
        .expect("edge-triggered test interest must be added");
    target.make_ready();
    let first = collect_one_event(&epoll);
    let registered_again = source.registered() == 2 && source.live() == 1;
    let waits_ok = epoll.register_waiter_wakers().is_ok();
    target.make_ready();
    let second = collect_one_event(&epoll);

    matches!(first, Ok((1, Some(0x62))))
        && registered_again
        && waits_ok
        && matches!(second, Ok((1, Some(0x62))))
}

#[cfg(all(test, not(axtest)))]
fn one_shot_queued_by_recheck_releases_its_lease_for_test() -> bool {
    let epoll = Epoll::new();
    let source = CountedSource::new();
    let target = CountedFile::new(true, &source);
    let target_file: Arc<dyn FileLike> = target.clone();

    // Readiness seen by the registration recheck queues the interest while
    // its lease has not been notified, so the lease is still armed when the
    // one-shot event is consumed.
    epoll
        .add_file_for_test(1, target_file, 0x63, EpollFlags::ONESHOT)
        .expect("one-shot test interest must be added");
    let armed = source.live() == 1;
    let event = collect_one_event(&epoll);

    armed && matches!(event, Ok((1, Some(0x63)))) && source.live() == 0
}

#[cfg(all(test, not(axtest)))]
fn closed_file_releases_its_armed_lease_for_test() -> bool {
    let epoll = Epoll::new();
    let source = CountedSource::new();
    let target = CountedFile::new(false, &source);
    let target_file: Arc<dyn FileLike> = target.clone();

    epoll
        .add_file_for_test(1, target_file.clone(), 0x64, EpollFlags::empty())
        .expect("level-triggered test interest must be added");
    let armed = source.live() == 1;
    drop(target_file);
    drop(target);

    armed && epoll.register_waiter_wakers().is_ok() && source.live() == 0
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn concurrent_reverse_add_is_serialized() {
        assert!(super::concurrent_reverse_add_is_serialized_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn epoll_notify_worker_flushes_deferred_wake() {
        assert!(super::epoll_notify_worker_flushes_deferred_wake_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn level_aliases_are_both_delivered() {
        assert!(super::level_aliases_are_both_delivered_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn exclusive_aliases_publish_only_one_interest() {
        assert!(super::exclusive_aliases_publish_only_one_interest_for_test());
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
    fn wake_during_registration_is_deferred() {
        assert!(super::wake_during_registration_is_deferred_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn requeues_readiness_observed_during_rearm() {
        assert!(super::epoll_requeues_readiness_observed_during_rearm_for_test());
    }
    #[cfg(all(test, not(axtest)))]
    #[test]
    fn armed_lease_survives_repeated_waits() {
        assert!(super::armed_lease_survives_repeated_waits_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn notified_lease_is_registered_again() {
        assert!(super::notified_lease_is_registered_again_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn one_shot_queued_by_recheck_releases_its_lease() {
        assert!(super::one_shot_queued_by_recheck_releases_its_lease_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn closed_file_releases_its_armed_lease() {
        assert!(super::closed_file_releases_its_armed_lease_for_test());
    }
}

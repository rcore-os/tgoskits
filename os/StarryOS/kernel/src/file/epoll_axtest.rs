//! Deterministic concurrency hooks for epoll kernel tests.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_errno::AxError;
use ax_kspin::SpinNoIrq;

use super::epoll::Epoll;

static EPOLL_ADD_TEST_BARRIER_ENABLED: AtomicBool = AtomicBool::new(false);
static EPOLL_ADD_TEST_BARRIER_ARRIVALS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn epoll_add_test_barrier() {
    if !EPOLL_ADD_TEST_BARRIER_ENABLED.load(Ordering::Acquire) {
        return;
    }

    EPOLL_ADD_TEST_BARRIER_ARRIVALS.fetch_add(1, Ordering::AcqRel);
    while EPOLL_ADD_TEST_BARRIER_ARRIVALS.load(Ordering::Acquire) < 2 {
        crate::task::yield_now();
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
        crate::task::spawn_kernel_thread(
            move || {
                results.lock()[0] = left.add_nested_for_test(1, right).err();
            },
            "epoll-axtest-left".into(),
        )
    };
    let right_task = {
        let left = Arc::clone(&left);
        let right = Arc::clone(&right);
        let results = Arc::clone(&results);
        crate::task::spawn_kernel_thread(
            move || {
                results.lock()[1] = right.add_nested_for_test(2, left).err();
            },
            "epoll-axtest-right".into(),
        )
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

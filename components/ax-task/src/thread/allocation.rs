//! Fallible allocation boundaries of the unpublished thread transaction.
use alloc::{boxed::Box, sync::Arc, vec::Vec};

use super::TaskError;

pub(crate) fn allocation_point() -> Result<(), TaskError> {
    #[cfg(feature = "fault-injection")]
    probe::allocation_point()?;
    Ok(())
}

#[cfg(feature = "fault-injection")]
pub use probe::ThreadAllocationProbe;

#[cfg(feature = "fault-injection")]
mod probe {
    use core::{
        marker::PhantomData,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    use super::TaskError;
    static OWNER: AtomicU64 = AtomicU64::new(0);
    static FAIL_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
    static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    /// Observes only the creating task's allocation boundaries in a real kernel.
    pub struct ThreadAllocationProbe {
        _not_send: PhantomData<*mut ()>,
    }
    impl ThreadAllocationProbe {
        /// Fails the zero-based allocation attempt; usize::MAX only observes.
        pub fn fail_at(attempt: usize) -> Result<Self, TaskError> {
            crate::thread::current::validate_blocking_context()?;
            let owner = crate::thread::current::current_thread_id()?.as_u64();
            OWNER
                .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| TaskError::ThreadBusy)?;
            FAIL_AT.store(attempt, Ordering::Release);
            ATTEMPTS.store(0, Ordering::Release);
            Ok(Self {
                _not_send: PhantomData,
            })
        }
        /// Records one runtime-provider allocation in the same transaction.
        /// This hook is compiled only for real-kernel fault-injection tests.
        pub fn allocation_point() -> Result<(), TaskError> {
            allocation_point()
        }
        /// Number of actual allocation boundaries reached by this creator.
        pub fn attempts(&self) -> usize {
            ATTEMPTS.load(Ordering::Acquire)
        }
    }
    impl Drop for ThreadAllocationProbe {
        fn drop(&mut self) {
            OWNER.store(0, Ordering::Release);
        }
    }
    pub(super) fn allocation_point() -> Result<(), TaskError> {
        let owner = OWNER.load(Ordering::Acquire);
        if owner == 0 || crate::thread::current::current_thread_id()?.as_u64() != owner {
            return Ok(());
        }
        assert!(
            crate::thread::current::validate_blocking_context().is_ok(),
            "thread allocation must not hold a scheduler or IRQ lock"
        );
        let attempt = ATTEMPTS.fetch_add(1, Ordering::AcqRel);
        if attempt == FAIL_AT.load(Ordering::Acquire) {
            return Err(TaskError::RuntimeFailure(
                crate::runtime::RuntimeStatus::NoMemory as u32,
            ));
        }
        Ok(())
    }
}

pub(crate) fn no_memory() -> TaskError {
    TaskError::RuntimeFailure(crate::runtime::RuntimeStatus::NoMemory as u32)
}

pub(crate) fn try_box<T>(value: T) -> Result<Box<T>, TaskError> {
    allocation_point()?;
    Box::try_new(value).map_err(|_| no_memory())
}

pub(crate) fn try_arc<T>(value: T) -> Result<Arc<T>, TaskError> {
    allocation_point()?;
    Arc::try_new(value).map_err(|_| no_memory())
}

pub(crate) fn try_vec<T>(capacity: usize) -> Result<Vec<T>, TaskError> {
    allocation_point()?;
    let mut items = Vec::new();
    items.try_reserve_exact(capacity).map_err(|_| no_memory())?;
    Ok(items)
}

pub(crate) fn empty_slots<T>(capacity: usize) -> Result<Vec<Option<T>>, TaskError> {
    let mut items = try_vec(capacity)?;
    items.resize_with(capacity, || None);
    Ok(items)
}

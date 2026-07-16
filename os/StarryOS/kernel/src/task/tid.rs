//! Starry-owned Linux thread ID allocation.

use core::sync::atomic::{AtomicU32, Ordering};

use ax_errno::{AxError, AxResult};
use starry_process::Pid;

/// Allocates Linux-visible TIDs without coupling them to scheduler slots.
///
/// IDs are deliberately not reused. This removes PID/TID ABA from the kernel's
/// user-ID tables while scheduler records independently use slot generations.
/// Exhaustion is reported instead of wrapping to a live or stale identifier.
struct UserTidAllocator {
    next: AtomicU32,
}

impl UserTidAllocator {
    const EXHAUSTED: u32 = 0;

    const fn new(first: Pid) -> Self {
        Self {
            next: AtomicU32::new(first),
        }
    }

    fn allocate(&self) -> AxResult<Pid> {
        let mut current = self.next.load(Ordering::Acquire);
        loop {
            if current == Self::EXHAUSTED {
                return Err(AxError::WouldBlock);
            }
            let next = current.checked_add(1).unwrap_or(Self::EXHAUSTED);
            match self.next.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(current),
                Err(observed) => current = observed,
            }
        }
    }
}

// PID 1 is reserved for the init process. Kernel helper threads do not consume
// Linux-visible identifiers.
static USER_TID_ALLOCATOR: UserTidAllocator = UserTidAllocator::new(2);

/// Allocates a Linux-visible TID independently from `ax-task::ThreadId`.
pub fn allocate_user_tid() -> AxResult<Pid> {
    USER_TID_ALLOCATOR.allocate()
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread, vec::Vec};

    use super::*;

    #[test]
    fn allocates_unique_monotonic_user_ids() {
        let allocator = UserTidAllocator::new(2);

        assert_eq!(allocator.allocate(), Ok(2));
        assert_eq!(allocator.allocate(), Ok(3));
    }

    #[test]
    fn reports_exhaustion_instead_of_reusing_an_id() {
        let allocator = UserTidAllocator::new(u32::MAX);

        assert_eq!(allocator.allocate(), Ok(u32::MAX));
        assert_eq!(allocator.allocate(), Err(AxError::WouldBlock));
    }

    #[test]
    fn concurrent_allocations_cover_one_unique_contiguous_range() {
        const WORKERS: usize = 8;
        const IDS_PER_WORKER: usize = 64;
        const FIRST: u32 = 100;

        let allocator = Arc::new(UserTidAllocator::new(FIRST));
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                let allocator = Arc::clone(&allocator);
                thread::spawn(move || {
                    (0..IDS_PER_WORKER)
                        .map(|_| allocator.allocate().unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut allocated: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        allocated.sort_unstable();

        let expected: Vec<_> = (FIRST..FIRST + (WORKERS * IDS_PER_WORKER) as u32).collect();
        assert_eq!(allocated, expected);
    }
}

//! Page-fault resolution and bounded reclaim policy.

use ax_errno::{AxError, AxResult};

/// Clean page-cache eviction supplied by the kernel adapter.
pub trait CleanPageEvictor {
    /// Evicts at most `max_pages` clean pages without waiting for writeback.
    fn evict_clean_pages(&self, max_pages: usize) -> usize;
}

/// Runs one operation and retries it at most once after bounded clean-page reclaim.
pub fn retry_after_clean_page_reclaim<T>(
    requested_pages: usize,
    evictor: &dyn CleanPageEvictor,
    mut operation: impl FnMut() -> AxResult<T>,
) -> AxResult<T> {
    let first = operation();
    if !matches!(first, Err(AxError::NoMemory)) {
        return first;
    }
    if requested_pages == 0 || evictor.evict_clean_pages(requested_pages) == 0 {
        return first;
    }
    operation()
}

/// Outcome of one user page-fault resolution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultOutcome {
    /// The mapping now satisfies the faulting access.
    Resolved,
    /// No VMA covers the fault address.
    NoMapping,
    /// A VMA exists but rejects the requested access.
    PermissionDenied,
    /// Physical memory remained unavailable after one bounded reclaim attempt.
    NoMemory,
    /// The backing page source failed.
    BackingError,
}

impl FaultOutcome {
    pub const fn is_resolved(self) -> bool {
        matches!(self, Self::Resolved)
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    struct TestEvictor {
        reclaimed: usize,
        calls: Cell<usize>,
    }

    impl CleanPageEvictor for TestEvictor {
        fn evict_clean_pages(&self, _max_pages: usize) -> usize {
            self.calls.set(self.calls.get() + 1);
            self.reclaimed
        }
    }

    #[test]
    fn retries_no_memory_only_once_after_successful_reclaim() {
        let evictor = TestEvictor {
            reclaimed: 1,
            calls: Cell::new(0),
        };
        let attempts = Cell::new(0);

        let result = retry_after_clean_page_reclaim(4, &evictor, || {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(AxError::NoMemory)
        });

        assert_eq!(result, Err(AxError::NoMemory));
        assert_eq!(attempts.get(), 2);
        assert_eq!(evictor.calls.get(), 1);
    }

    #[test]
    fn does_not_reclaim_for_backing_errors() {
        let evictor = TestEvictor {
            reclaimed: 1,
            calls: Cell::new(0),
        };
        let result =
            retry_after_clean_page_reclaim(1, &evictor, || Err::<(), _>(AxError::BadAddress));

        assert_eq!(result, Err(AxError::BadAddress));
        assert_eq!(evictor.calls.get(), 0);
    }
}

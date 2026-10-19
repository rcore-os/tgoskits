use alloc::sync::Arc;
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::os::memory::PAGE_SIZE;

const INITIAL_READAHEAD_PAGES: usize = 4;
pub(super) const MAX_READAHEAD_PAGES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReadAheadPlan {
    pub(super) window_pages: usize,
}

pub(super) struct ReadAheadState {
    next_offset: u64,
    window_pages: usize,
    initialized: bool,
    sequential: bool,
    loaded: Option<Arc<ReadAheadWindow>>,
}

struct ReadAheadWindow {
    pages: Range<u64>,
    pending: AtomicBool,
}

/// Owns one speculative attempt. Releasing it re-arms its original window,
/// including startup, queue admission, fill contention and backing I/O errors.
/// A replacement window has a different allocation, so stale completions
/// cannot clear a newer window's reservation after a seek or demand fill.
pub(super) struct PrefetchRequest {
    source: Arc<ReadAheadWindow>,
    pub(super) pages: Range<u64>,
}

impl Drop for PrefetchRequest {
    fn drop(&mut self) {
        // This bit only excludes duplicate attempts; page publication and the
        // current window are protected by their existing cache/state locks.
        self.source.pending.store(false, Ordering::Relaxed);
    }
}

impl ReadAheadState {
    pub(super) const fn new() -> Self {
        Self {
            next_offset: 0,
            window_pages: INITIAL_READAHEAD_PAGES,
            initialized: false,
            sequential: false,
            loaded: None,
        }
    }

    pub(super) fn plan(&mut self, offset: u64, end: u64) -> ReadAheadPlan {
        let sequential = if self.initialized {
            offset == self.next_offset
        } else {
            offset == 0
        };
        self.sequential = sequential;
        if !sequential {
            self.loaded = None;
        }
        let first_page = offset / PAGE_SIZE as u64;
        let end_page = end.div_ceil(PAGE_SIZE as u64);
        let requested_pages =
            usize::try_from(end_page.saturating_sub(first_page)).unwrap_or(usize::MAX);
        let window_pages = if sequential {
            self.window_pages
                .max(requested_pages)
                .min(MAX_READAHEAD_PAGES)
        } else {
            requested_pages.min(MAX_READAHEAD_PAGES)
        };

        self.next_offset = end;
        self.initialized = true;
        self.window_pages = if sequential {
            self.window_pages.saturating_mul(2).min(MAX_READAHEAD_PAGES)
        } else {
            INITIAL_READAHEAD_PAGES
        };
        ReadAheadPlan { window_pages }
    }

    pub(super) fn loaded(&mut self, range: Range<u64>) {
        if self
            .loaded
            .as_ref()
            .is_none_or(|old| old.pages.start <= range.start)
        {
            self.loaded = Some(Arc::new(ReadAheadWindow {
                pages: range,
                pending: AtomicBool::new(false),
            }));
        }
    }

    pub(super) fn prefetched(&mut self, attempt: &PrefetchRequest) {
        if self
            .loaded
            .as_ref()
            .is_some_and(|window| Arc::ptr_eq(window, &attempt.source))
        {
            self.loaded(attempt.pages.clone());
        }
    }

    /// Reserve one retryable attempt while demand is inside the resident window.
    pub(super) fn take_prefetch(&mut self, page: u32, file_pages: u64) -> Option<PrefetchRequest> {
        if !self.sequential {
            return None;
        }
        let file_pages = file_pages.min(u64::from(u32::MAX) + 1);
        let source = self.loaded.as_ref()?;
        let window = &source.pages;
        let page = u64::from(page);
        let trigger = window.start + (window.end - window.start) / 2;
        if !window.contains(&page) || page < trigger || window.end >= file_pages {
            return None;
        }
        let next = window.end;
        let count = self.window_pages.max(INITIAL_READAHEAD_PAGES) as u64;
        if source.pending.swap(true, Ordering::Relaxed) {
            return None;
        }
        Some(PrefetchRequest {
            source: Arc::clone(source),
            pages: next..next.saturating_add(count).min(file_pages),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{INITIAL_READAHEAD_PAGES, MAX_READAHEAD_PAGES, ReadAheadState};
    use crate::os::memory::PAGE_SIZE;

    #[test]
    fn sequential_reads_grow_a_bounded_window_and_random_reads_reset_it() {
        let mut state = ReadAheadState::new();

        assert_eq!(
            state.plan(0, PAGE_SIZE as u64).window_pages,
            INITIAL_READAHEAD_PAGES
        );
        assert_eq!(
            state
                .plan(PAGE_SIZE as u64, (PAGE_SIZE * 2) as u64)
                .window_pages,
            INITIAL_READAHEAD_PAGES * 2
        );
        assert_eq!(
            state
                .plan(17 * PAGE_SIZE as u64, 18 * PAGE_SIZE as u64)
                .window_pages,
            1
        );
        assert_eq!(
            state
                .plan(18 * PAGE_SIZE as u64, 19 * PAGE_SIZE as u64)
                .window_pages,
            INITIAL_READAHEAD_PAGES
        );

        state.loaded(18..22);
        assert!(state.take_prefetch(19, 100).is_none());
        let attempt = state.take_prefetch(20, 100).unwrap();
        assert_eq!(attempt.pages, 22..30);
        assert!(state.take_prefetch(21, 100).is_none());
        drop(attempt);
        let retry = state
            .take_prefetch(21, 100)
            .expect("a dropped attempt must re-arm");
        assert_eq!(retry.pages, 22..30);
        state.loaded(22..30);
        let newer = state.take_prefetch(26, 100).unwrap();
        drop(retry);
        assert!(
            state.take_prefetch(27, 100).is_none(),
            "an old attempt must not release the new window"
        );
        drop(newer);

        let limit = u64::from(u32::MAX) + 1;
        state.loaded((limit - 4)..limit);
        assert!(
            state.take_prefetch(u32::MAX, limit + 4).is_none(),
            "the next window must not wrap its first page back to zero"
        );
        state.plan(80 * PAGE_SIZE as u64, 81 * PAGE_SIZE as u64);
        state.loaded(80..81);
        assert!(
            state.take_prefetch(80, 100).is_none(),
            "random demand must not launch speculation"
        );
        state.plan(18 * PAGE_SIZE as u64, 19 * PAGE_SIZE as u64);

        for page in 19..64 {
            let _ = state.plan(page * PAGE_SIZE as u64, (page + 1) * PAGE_SIZE as u64);
        }
        assert_eq!(
            state
                .plan(64 * PAGE_SIZE as u64, 65 * PAGE_SIZE as u64)
                .window_pages,
            MAX_READAHEAD_PAGES
        );
    }
}

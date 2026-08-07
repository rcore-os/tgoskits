use crate::os::memory::PAGE_SIZE;

const INITIAL_READAHEAD_PAGES: usize = 4;
const MAX_READAHEAD_PAGES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReadAheadPlan {
    pub(super) window_pages: usize,
}

pub(super) struct ReadAheadState {
    next_offset: u64,
    window_pages: usize,
    initialized: bool,
}

impl ReadAheadState {
    pub(super) const fn new() -> Self {
        Self {
            next_offset: 0,
            window_pages: INITIAL_READAHEAD_PAGES,
            initialized: false,
        }
    }

    pub(super) fn plan(&mut self, offset: u64, end: u64) -> ReadAheadPlan {
        let sequential = if self.initialized {
            offset == self.next_offset
        } else {
            offset == 0
        };
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

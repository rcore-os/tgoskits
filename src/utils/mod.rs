mod arch;

pub mod cache {
    #[allow(unused)]
    /// Invalidate the data cache.
    pub unsafe fn cache_invalidate_d(start: usize, len: usize) {
        unsafe { super::arch::cache::cache_invalidate_d(start, len) };
    }
    /// Clean and invalidate the data cache.
    pub unsafe fn cache_clean_invalidate_d(start: usize, len: usize) {
        unsafe { super::arch::cache::cache_clean_invalidate_d(start, len) };
    }
}

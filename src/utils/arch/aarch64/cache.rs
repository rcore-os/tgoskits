use core::arch::global_asm;

global_asm!(include_str!("cache.S"));

unsafe extern "C" {
    /// Invalidate the data cache.
    pub unsafe fn cache_invalidate_d(start: usize, len: usize);
    /// Clean and invalidate the data cache.
    pub unsafe fn cache_clean_invalidate_d(start: usize, len: usize);
}

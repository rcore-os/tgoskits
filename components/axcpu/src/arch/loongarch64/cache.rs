//! Local cache operations.

pub use super::asm::flush_icache_all;

/// Completes data-side preparation for subsequent local instruction synchronization.
/// This architecture needs no separate data-cache clean for coherent CPU stores;
/// callers must still perform `flush_icache_all` before executing modified text.
///
/// # Safety
/// The range must remain mapped; the caller coordinates concurrent text writes
/// and execution, including the subsequent instruction synchronization.
#[inline]
pub unsafe fn clean_dcache_range_to_pou(_range: crate::cache::CacheRange) {}

/// Writes back and invalidates data-cache lines intersecting the byte range.
///
/// The supported LoongArch cache geometry uses 64-byte lines. The starting
/// address is rounded down; completion includes a full `dbar 0` barrier.
/// An empty range performs no cache operation.
///
/// # Safety
/// The caller must own the cache lines covering the range, keep them mapped,
/// and exclude conflicting CPU/device writes until completion. The range must
/// not wrap the virtual address space.
pub unsafe fn clean_invalidate_dcache_range(range: crate::cache::CacheRange) {
    if range.is_empty() {
        return;
    }
    range.for_each_line(64, |line| {
        // SAFETY: the caller owns each mapped line; the checked range cannot wrap.
        unsafe {
            core::arch::asm!("cacop 0x19, {}, 0", in(reg) line, options(nostack));
        }
    });
    // SAFETY: complete maintenance before returning the ownership window.
    unsafe {
        core::arch::asm!("dbar 0", options(nostack));
    }
}

//! Local cache operations.

pub use super::asm::{dcache_line_size_from_ctr, flush_icache_all, icache_line_size_from_ctr};

/// Data-cache maintenance performed to the point of coherency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataCacheOperation {
    /// Writes dirty data back while retaining valid cache lines.
    Clean,
    /// Discards cached contents without writing them back.
    Invalidate,
    /// Writes dirty contents back and discards the cache lines.
    CleanInvalidate,
}

/// Maintains every cache line intersecting a checked virtual byte range.
/// Completion includes a system DSB and instruction synchronization.
///
/// # Safety
/// Every intersected cache line must be mapped and owned for the operation.
/// Invalidation must not discard live dirty data or race CPU/device accesses.
/// The caller owns DMA transfer direction, aliases and platform coherency.
pub unsafe fn maintain_dcache_to_poc(
    operation: DataCacheOperation,
    range: crate::cache::CacheRange,
) {
    if range.is_empty() {
        return;
    }
    range.for_each_line(dcache_line_size_from_ctr(), |line| {
        // SAFETY: the caller retains every complete intersected line and the
        // checked range iterator cannot wrap into an unrelated address region.
        unsafe {
            match operation {
                DataCacheOperation::Clean => core::arch::asm!("dc cvac, {}", in(reg) line),
                DataCacheOperation::Invalidate => core::arch::asm!("dc ivac, {}", in(reg) line),
                DataCacheOperation::CleanInvalidate => {
                    core::arch::asm!("dc civac, {}", in(reg) line)
                }
            }
        }
    });
    // SAFETY: complete the cache operations before returning ownership.
    unsafe { core::arch::asm!("dsb sy; isb", options(nostack)) };
}

/// Cleans the checked range to the point of unification and completes the writes.
/// Call `flush_icache_all` before executing the modified instructions.
///
/// # Safety
/// Every intersected cache line must remain mapped and accessible until completion.
/// The caller must coordinate concurrent modifications and instruction execution.
pub unsafe fn clean_dcache_range_to_pou(range: crate::cache::CacheRange) {
    if range.is_empty() {
        return;
    }
    range.for_each_line(dcache_line_size_from_ctr(), |line| {
        // SAFETY: the caller retains the mapped lines; the checked iterator
        // visits the final line without rounding the endpoint past usize::MAX.
        unsafe { core::arch::asm!("dc cvau, {}", in(reg) line, options(nostack)) };
    });
    // SAFETY: complete the data-side publication before instruction invalidation.
    unsafe { core::arch::asm!("dsb ish", options(nostack)) };
}

// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// T-Head cache maintenance migrated from someboot (周睿).
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

/// T-Head physical-address data-cache operation.
#[cfg(feature = "riscv-thead-mae")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TheadDataCacheOperation {
    /// Writes dirty data back without discarding the line.
    Clean,
    /// Writes dirty data back and discards the line.
    CleanInvalidate,
}

/// Maintains physical cache lines and completes with T-Head sync.is.
/// Platform DMA fences, direction and alias ownership remain caller policy.
///
/// # Safety
/// Execute on a CPU implementing the T-Head cache instructions and 64-byte
/// lines. The caller owns every intersecting physical line and must exclude
/// conflicting CPU/device writes until the operation completes.
#[cfg(feature = "riscv-thead-mae")]
pub unsafe fn maintain_thead_dcache(
    operation: TheadDataCacheOperation,
    range: crate::cache::PhysicalCacheRange,
) {
    if range.is_empty() {
        return;
    }
    range.for_each_line(64, |line| {
        // SAFETY: the owner supplied a checked physical range on supported hardware.
        unsafe {
            match operation {
                TheadDataCacheOperation::Clean => {
                    core::arch::asm!(".long 0x0295000b", in("a0") line, options(nostack))
                }
                TheadDataCacheOperation::CleanInvalidate => {
                    core::arch::asm!(".long 0x02b5000b", in("a0") line, options(nostack))
                }
            }
        }
    });
    // SAFETY: complete the implemented vendor cache instructions before return.
    unsafe {
        core::arch::asm!(".long 0x01b0000b", options(nostack));
    }
}

//! Cache-block operations on exclusively owned ordinary RAM.

use core::arch::asm;

use ax_memory_addr::PAGE_SIZE_4K;

/// Clears one aligned 4 KiB page using the current CPU's cache-zero operation.
///
/// Returns `false`, without changing memory, if the page is unaligned, DC ZVA
/// is prohibited, or the CPU's zero block is larger than the page. The caller
/// may then use ordinary stores. This does not publish a PTE or maintain DMA
/// visibility, and never changes the CPU's access-control registers.
///
/// # Safety
///
/// `page` must address a writable 4 KiB allocation of Normal memory that the
/// caller owns exclusively, not device memory or a published shared page.
/// The caller must prevent migration from the register read until return, so
/// the block size always describes the CPU executing DC ZVA. The execution
/// environment must allow reads of DCZID_EL0 at the current exception level.
pub unsafe fn try_zero_page(page: *mut u8) -> bool {
    if !(page as usize).is_multiple_of(PAGE_SIZE_4K) {
        return false;
    }
    let dczid: u64;
    // SAFETY: the caller provides the privileged register-access context.
    unsafe {
        asm!("mrs {id}, dczid_el0", id = out(reg) dczid, options(nomem, nostack, preserves_flags));
    }
    let Some(block_size) = zero_block_size(dczid) else {
        return false;
    };
    for offset in (0..PAGE_SIZE_4K).step_by(block_size) {
        // SAFETY: block_size is a power of two no larger than the aligned page.
        // Every zeroed block lies entirely in the exclusive allocation. Default
        // asm memory effects keep initialization before subsequent publication.
        unsafe {
            asm!("dc zva, {address}", address = in(reg) page.add(offset), options(nostack, preserves_flags));
        }
    }
    true
}

fn zero_block_size(dczid: u64) -> Option<usize> {
    const PROHIBITED: u64 = 1 << 4;
    if dczid & PROHIBITED != 0 {
        return None;
    }
    let block_size = 4usize << (dczid & 0xf);
    (block_size <= PAGE_SIZE_4K).then_some(block_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_zero_geometry_covers_every_block_size_encoding() {
        for encoding in 0..=15 {
            let expected = (encoding <= 10).then_some(4usize << encoding);
            assert_eq!(zero_block_size(encoding), expected);
        }
    }

    #[test]
    fn prohibited_cache_zero_never_produces_a_geometry() {
        for encoding in 0..=15 {
            assert_eq!(zero_block_size((1 << 4) | encoding), None);
        }
    }
}

//! Initialization of exclusively owned, unpublished data frames.

/// Clears an allocation before any mapping or shared owner can observe it.
///
/// # Safety
///
/// `start` must be the writable direct-map address of an exclusively owned
/// Normal RAM allocation spanning `bytes`. It must not name MMIO, DMA-owned
/// memory, or a frame already reachable through a published PTE.
pub(super) unsafe fn clear_owned(start: *mut u8, bytes: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        use ax_memory_addr::PAGE_SIZE_4K;
        if (start as usize).is_multiple_of(PAGE_SIZE_4K) && bytes == PAGE_SIZE_4K {
            // DCZID describes this CPU. Keep the task here from the register
            // read through the last cache-zero instruction on heterogeneous CPUs.
            let _pin = crate::sync::PreemptGuard::new();
            for offset in (0..bytes).step_by(PAGE_SIZE_4K) {
                // SAFETY: each complete page lies in the exclusive allocation;
                // the preemption guard prevents migration throughout the call.
                let page = unsafe { start.add(offset) };
                if !unsafe { ax_cpu::cache::try_zero_page(page) } {
                    // SAFETY: the capability refused without modifying memory.
                    unsafe { core::ptr::write_bytes(page, 0, PAGE_SIZE_4K) };
                }
            }
            return;
        }
    }
    // SAFETY: the caller owns the complete writable byte range.
    unsafe { core::ptr::write_bytes(start, 0, bytes) };
}

#[cfg(axtest)]
mod tests {
    use ax_memory_addr::PAGE_SIZE_4K;
    use ax_runtime::hal::mem::phys_to_virt;

    #[axtest::axtest]
    fn clearing_an_owned_page_preserves_its_neighbor_frames() {
        let frame = super::super::alloc_frame(false, PAGE_SIZE_4K * 4).unwrap();
        let address = phys_to_virt(frame).as_mut_ptr();
        // SAFETY: this test exclusively owns all four unpublished allocator
        // pages until dealloc_frame, and no device or page table uses them.
        unsafe {
            core::ptr::write_bytes(address, 0x5a, PAGE_SIZE_4K * 4);
            super::clear_owned(address.add(PAGE_SIZE_4K), PAGE_SIZE_4K);
            let bytes = core::slice::from_raw_parts(address, PAGE_SIZE_4K * 4);
            assert!(bytes[..PAGE_SIZE_4K].iter().all(|byte| *byte == 0x5a));
            assert!(bytes[PAGE_SIZE_4K..PAGE_SIZE_4K * 2].iter().all(|byte| *byte == 0));
            assert!(bytes[PAGE_SIZE_4K * 2..].iter().all(|byte| *byte == 0x5a));
        }
        super::super::dealloc_frame(frame, PAGE_SIZE_4K * 4);
    }
}

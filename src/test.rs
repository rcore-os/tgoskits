use memory_addr::{pa, va};

/// A demonstration of the `memory` API implementation.
#[crate::api_mod_impl(crate::memory)]
mod memory_impl {
    use core::sync::atomic::AtomicUsize;
    use memory_addr::{PhysAddr, VirtAddr, pa, va};

    static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
    static RETURNED_SUM: AtomicUsize = AtomicUsize::new(0);
    pub const VA_PA_OFFSET: usize = 0x1000;

    extern fn alloc_frame() -> Option<PhysAddr> {
        let value = ALLOCATED.fetch_add(1, core::sync::atomic::Ordering::SeqCst);

        Some(pa!(value * 0x1000))
    }

    extern fn alloc_contiguous_frames(
        _num_frames: usize,
        _frame_align_pow2: usize,
    ) -> Option<PhysAddr> {
        unimplemented!();
    }

    extern fn dealloc_frame(addr: PhysAddr) {
        RETURNED_SUM.fetch_add(addr.as_usize(), core::sync::atomic::Ordering::SeqCst);
    }

    extern fn dealloc_contiguous_frames(_first_addr: PhysAddr, _num_frames: usize) {
        unimplemented!();
    }

    /// Get the sum of all returned physical addresses.
    ///
    /// Note that this function demonstrates that non-API functions work well in a module with the `api_mod_impl` attribute.
    pub fn get_returned_sum() -> usize {
        RETURNED_SUM.load(core::sync::atomic::Ordering::SeqCst)
    }

    pub fn clear() {
        ALLOCATED.store(0, core::sync::atomic::Ordering::SeqCst);
        RETURNED_SUM.store(0, core::sync::atomic::Ordering::SeqCst);
    }

    extern fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        va!(addr.as_usize() + VA_PA_OFFSET) // Example implementation
    }

    extern fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
        pa!(addr.as_usize() - VA_PA_OFFSET) // Example implementation
    }
}

#[test]
pub fn test_memory() {
    use crate::memory;

    memory_impl::clear();

    let frame1 = memory::alloc_frame();
    let frame2 = memory::alloc_frame();
    let frame3 = memory::alloc_frame();

    assert_eq!(frame1, Some(pa!(0x0)));
    assert_eq!(frame2, Some(pa!(0x1000)));
    assert_eq!(frame3, Some(pa!(0x2000)));

    memory::dealloc_frame(frame2.unwrap());
    assert_eq!(memory_impl::get_returned_sum(), 0x1000);
    memory::dealloc_frame(frame3.unwrap());
    assert_eq!(memory_impl::get_returned_sum(), 0x3000);
    memory::dealloc_frame(frame1.unwrap());
    assert_eq!(memory_impl::get_returned_sum(), 0x3000);

    assert_eq!(memory::phys_to_virt(pa!(0)), va!(memory_impl::VA_PA_OFFSET));
    assert_eq!(memory::virt_to_phys(va!(memory_impl::VA_PA_OFFSET)), pa!(0));
}

#[test]
pub fn test_memory_phys_frame() {
    use crate::memory;
    use crate::memory::PhysFrame;

    memory_impl::clear();

    let _ = memory::alloc_frame();
    let frame1 = PhysFrame::alloc().unwrap();
    let frame2 = PhysFrame::alloc().unwrap();
    let frame3 = PhysFrame::alloc().unwrap();

    assert_eq!(frame1.start_paddr(), pa!(0x1000));
    assert_eq!(frame2.start_paddr(), pa!(0x2000));
    assert_eq!(frame3.start_paddr(), pa!(0x3000));

    drop(frame2);
    assert_eq!(memory_impl::get_returned_sum(), 0x2000);
    drop(frame3);
    assert_eq!(memory_impl::get_returned_sum(), 0x5000);
    drop(frame1);
    assert_eq!(memory_impl::get_returned_sum(), 0x6000);
}

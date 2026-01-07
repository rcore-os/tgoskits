use memory_addr::{pa, va};

mod memory_impl {
    extern crate std; // in test only

    use memory_addr::{PhysAddr, VirtAddr, pa, va};
    use std::sync::{
        Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    };

    static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
    static RETURNED_SUM: AtomicUsize = AtomicUsize::new(0);
    static LOCK: Mutex<()> = Mutex::new(());
    pub const VA_PA_OFFSET: usize = 0x1000;

    pub struct MemoryIfImpl;

    #[crate::api_impl]
    impl crate::memory::MemoryIf for MemoryIfImpl {
        fn alloc_frame() -> Option<PhysAddr> {
            let value = ALLOCATED.fetch_add(1, Ordering::Relaxed);

            Some(pa!(value * 0x1000))
        }

        fn alloc_contiguous_frames(
            _num_frames: usize,
            _frame_align_pow2: usize,
        ) -> Option<PhysAddr> {
            unimplemented!();
        }

        fn dealloc_frame(addr: PhysAddr) {
            RETURNED_SUM.fetch_add(addr.as_usize(), Ordering::Relaxed);
        }

        fn dealloc_contiguous_frames(_first_addr: PhysAddr, _num_frames: usize) {
            unimplemented!();
        }

        fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
            va!(addr.as_usize() + VA_PA_OFFSET) // Example implementation
        }

        fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
            pa!(addr.as_usize() - VA_PA_OFFSET) // Example implementation
        }
    }

    /// Get the sum of all returned physical addresses.
    ///
    /// Note that this function demonstrates that non-API functions work well in a module with the `api_mod_impl` attribute.
    pub fn get_returned_sum() -> usize {
        RETURNED_SUM.load(Ordering::Relaxed)
    }

    /// Start a test by acquiring the lock and resetting the internal state.
    pub fn enter_test() -> MutexGuard<'static, ()> {
        let guard = LOCK.lock().unwrap();
        ALLOCATED.store(0, Ordering::Relaxed);
        RETURNED_SUM.store(0, Ordering::Relaxed);
        guard
    }
}

#[test]
pub fn test_memory() {
    use crate::memory;

    let guard = memory_impl::enter_test();

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

    drop(guard);
}

#[test]
pub fn test_memory_phys_frame() {
    use crate::memory::{self, PhysFrame};

    let guard = memory_impl::enter_test();

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

    drop(guard);
}

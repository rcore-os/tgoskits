//! Real ArceOS control-page leases shared by CPU integration cases.

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_alloc::GlobalPage;
use ax_cpu::{PhysAddr, virtualization::ControlMemory};

pub(crate) static LIVE_PAGES: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct ControlPages {
    pages: GlobalPage,
    physical: PhysAddr,
    offset: usize,
}

impl ControlPages {
    pub(crate) fn new(offset: usize) -> Self {
        assert!(offset < 4096);
        let mut memory = Self::allocate(1);
        memory.offset = offset;
        memory
    }

    pub(crate) fn allocate(count: usize) -> Self {
        let mut pages = GlobalPage::alloc_contiguous(count, 4096).unwrap();
        pages.zero();
        let physical = ax_hal::mem::virt_to_phys(pages.start_vaddr());
        for offset in (0..pages.size()).step_by(4096) {
            assert_eq!(
                ax_hal::mem::virt_to_phys(pages.start_vaddr() + offset),
                physical + offset
            );
        }
        LIVE_PAGES.fetch_add(count, Ordering::Relaxed);
        Self {
            pages,
            physical,
            offset: 0,
        }
    }
}

// SAFETY: the real allocator supplies exclusive initialized WB pages. The
// constructor verifies their physical contiguity through the permanent direct
// map. No Clone or external owner exists; forgetting this lease also forgets
// GlobalPage, so the allocation remains alive after failed hardware retirement.
unsafe impl ControlMemory for ControlPages {
    fn physical_address(&self) -> PhysAddr {
        self.physical + self.offset
    }
    fn virtual_address(&self) -> NonNull<u8> {
        NonNull::new((self.pages.start_vaddr() + self.offset).as_mut_ptr()).unwrap()
    }
    fn byte_len(&self) -> usize {
        self.pages.size() - self.offset
    }
}

impl Drop for ControlPages {
    fn drop(&mut self) {
        LIVE_PAGES.fetch_sub(self.pages.size() / 4096, Ordering::Relaxed);
    }
}

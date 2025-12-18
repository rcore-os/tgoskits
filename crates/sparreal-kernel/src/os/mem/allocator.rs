use core::{
    alloc::GlobalAlloc,
    ptr::{NonNull, null_mut},
};

use alloc::boxed::Box;
use buddy_system_allocator::Heap;
use page_table_generic::FrameAllocator;
use spin::Mutex;

use crate::{
    hal::al::memory::page_size,
    os::{
        irq::NoIrqGuard,
        mem::address::{PhysAddr, VirtAddr},
    },
};

#[cfg(target_os = "none")]
#[global_allocator]
pub(super) static KERNEL_MEMORY_ALLOCATOR: KernelMemoryAllocator = KernelMemoryAllocator::new();

fn page_frame_layout() -> core::alloc::Layout {
    core::alloc::Layout::from_size_align(page_size(), page_size()).unwrap()
}

#[derive(Clone, Copy)]
pub struct KernelAllocator;

impl FrameAllocator for KernelAllocator {
    fn alloc_frame(&self) -> Option<page_table_generic::PhysAddr> {
        kernel_memory_allocator()
            .lock_heap32()
            .alloc(page_frame_layout())
            .ok()
            .map(|nn| {
                let virt = VirtAddr::from(nn);
                let phys: PhysAddr = virt.into();
                phys.raw().into()
            })
    }

    fn dealloc_frame(&self, frame: page_table_generic::PhysAddr) {
        let phys = PhysAddr::new(frame.raw());
        let virt: VirtAddr = phys.into();
        let ptr = virt.as_mut_ptr();
        let nn = unsafe { NonNull::new_unchecked(ptr) };
        kernel_memory_allocator()
            .lock_heap32()
            .dealloc(nn, page_frame_layout());
    }

    fn phys_to_virt(&self, paddr: page_table_generic::PhysAddr) -> *mut u8 {
        let phys = PhysAddr::new(paddr.raw());
        let virt: VirtAddr = phys.into();
        virt.as_mut_ptr()
    }
}

/// 获取全局内核内存分配器实例
pub fn kernel_memory_allocator() -> &'static KernelMemoryAllocator {
    #[cfg(target_os = "none")]
    {
        &KERNEL_MEMORY_ALLOCATOR
    }
    #[cfg(not(target_os = "none"))]
    {
        // 对于非 none 目标，提供一个空实现
        use core::sync::atomic::{AtomicPtr, Ordering};

        static EMPTY_ALLOCATOR: AtomicPtr<KernelMemoryAllocator> =
            AtomicPtr::new(core::ptr::null_mut());

        let ptr = EMPTY_ALLOCATOR.load(Ordering::Acquire);
        if ptr.is_null() {
            let allocator = Box::leak(Box::new(KernelMemoryAllocator::new()));
            match EMPTY_ALLOCATOR.compare_exchange(
                core::ptr::null_mut(),
                allocator,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => allocator,
                Err(existing) => unsafe { &*existing },
            }
        } else {
            unsafe { &*ptr }
        }
    }
}

pub struct KernelMemoryAllocator {
    low_address_heap: Mutex<Heap<32>>,
    high_address_heap: Mutex<Heap<64>>,
}

impl KernelMemoryAllocator {
    pub const fn new() -> Self {
        Self {
            low_address_heap: Mutex::new(Heap::empty()),
            high_address_heap: Mutex::new(Heap::empty()),
        }
    }

    pub fn add_memory_region(&self, memory: &mut [u8]) {
        let range = memory.as_mut_ptr_range();
        let start = range.start as usize;
        let end = range.end as usize;

        if Self::address_range_fits_in_32bit(start, end) {
            let mut heap32 = self.low_address_heap.lock();
            unsafe { heap32.add_to_heap(start, end) };
        } else {
            let mut heap64 = self.high_address_heap.lock();
            unsafe { heap64.add_to_heap(start, end) };
        }
    }

    pub(crate) fn lock_heap32(&self) -> spin::MutexGuard<'_, Heap<32>> {
        self.low_address_heap.lock()
    }

    pub(crate) fn lock_heap64(&self) -> spin::MutexGuard<'_, Heap<64>> {
        self.high_address_heap.lock()
    }

    // pub(crate) unsafe fn alloc_with_mask(
    //     &self,
    //     layout: core::alloc::Layout,
    //     dma_mask: u64,
    // ) -> *mut u8 {
    //     let guard = NoIrqGuard::new();
    //     let result = if dma_mask <= u32::MAX as u64 {
    //         Self::try_alloc(&self.heap32, layout)
    //     } else {
    //         Self::try_alloc(&self.heap64, layout).or_else(|| Self::try_alloc(&self.heap32, layout))
    //     };
    //     drop(guard);

    //     result.map_or(null_mut(), |ptr| ptr.as_ptr())
    // }

    #[inline]
    fn try_alloc<const BITS: usize>(
        heap: &Mutex<Heap<BITS>>,
        layout: core::alloc::Layout,
    ) -> Option<NonNull<u8>> {
        let mut guard = heap.lock();
        guard.alloc(layout).ok()
    }

    #[inline]
    fn address_range_fits_in_32bit(start: usize, end: usize) -> bool {
        if start >= end {
            return false;
        }

        let last = end - 1;

        let ps = PhysAddr::from(VirtAddr::from(start));
        let pe = PhysAddr::from(VirtAddr::from(last));

        let limit = PhysAddr::from(u32::MAX as usize);
        ps <= limit && pe <= limit
    }

    #[inline]
    fn pointer_fits_in_32bit(ptr: *mut u8) -> bool {
        let phys = PhysAddr::from(VirtAddr::from(ptr as usize));
        phys <= PhysAddr::from(u32::MAX as usize)
    }
}

unsafe impl GlobalAlloc for KernelMemoryAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let guard = NoIrqGuard::new();
        let result = Self::try_alloc(&self.high_address_heap, layout)
            .or_else(|| Self::try_alloc(&self.low_address_heap, layout));
        drop(guard);

        result.map_or(null_mut(), |ptr| ptr.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let guard = NoIrqGuard::new();
        let nn = unsafe { NonNull::new_unchecked(ptr) };

        if Self::pointer_fits_in_32bit(ptr) {
            self.low_address_heap.lock().dealloc(nn, layout);
        } else {
            self.high_address_heap.lock().dealloc(nn, layout);
        }
        drop(guard);
    }
}

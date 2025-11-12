use core::{alloc::Layout, cell::UnsafeCell};

use num_align::NumAlign;
use os_helper::memory::{MemoryDescriptor, MemoryType};
use page_table_generic::FrameAllocator;

use crate::{ArchTrait, mem::page_size};

struct SimpleAllocator {
    start: usize,
    current: usize, // 当前分配位置
}

impl SimpleAllocator {
    const fn new() -> Self {
        SimpleAllocator {
            start: 0,
            current: 0,
        }
    }

    unsafe fn init(&mut self, kernel_end: usize) {
        self.start = kernel_end;
        self.current = kernel_end;
    }

    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        unsafe {
            let start = self.current.align_up(layout.align()) as *mut u8;
            let end = start.add(layout.size());
            self.current = end as usize;
            start
        }
    }
}

/// 单线程内存分配器
struct Allocator(UnsafeCell<SimpleAllocator>);
unsafe impl Sync for Allocator {}
unsafe impl Send for Allocator {}

static RAM_ALLOC: Allocator = Allocator(UnsafeCell::new(SimpleAllocator::new()));

#[derive(Clone, Copy)]
pub struct Ram;

impl Ram {
    pub fn current(&self) -> *mut u8 {
        unsafe { (*RAM_ALLOC.0.get()).current as _ }
    }

    pub fn alloc(&self, layout: Layout) -> Option<*mut u8> {
        Some(unsafe { (*RAM_ALLOC.0.get()).alloc(layout) })
    }
}

impl FrameAllocator for Ram {
    fn alloc_frame(&self) -> Option<page_table_generic::PhysAddr> {
        self.alloc(unsafe { Layout::from_size_align_unchecked(page_size(), page_size()) })
            .map(|ptr| (ptr as usize).into())
    }

    fn dealloc_frame(&self, _frame: page_table_generic::PhysAddr) {}

    fn phys_to_virt(&self, paddr: page_table_generic::PhysAddr) -> *mut u8 {
        paddr.raw() as *mut u8
    }
}

pub fn init() {
    let kernel_end = crate::arch::Arch::kernel_code().as_ptr_range().end as usize;
    unsafe {
        (*RAM_ALLOC.0.get()).init(kernel_end);
    }
}

pub fn current() -> *mut u8 {
    Ram {}.current() as _
}

pub fn to_rsvd_memory_descriptor() -> MemoryDescriptor {
    let start = unsafe { RAM_ALLOC.0.get().as_ref().unwrap().start };

    MemoryDescriptor {
        physical_start: start,
        size_in_bytes: current() as usize - start,
        memory_type: MemoryType::Reserved,
    }
}

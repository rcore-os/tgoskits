use core::{alloc::Layout, ops::Range};

use kernutil::memory::{MemoryDescriptor, MemoryType};
use num_align::NumAlign;
use page_table_generic::FrameAllocator;

use crate::mem::{add_memory_descriptor, page_size};

/// RAM 分配器的起始地址
static mut RAM_START: usize = 0;

/// RAM 分配器的结束地址
static mut RAM_END: usize = 0;

/// 当前分配位置
static mut RAM_CURRENT: usize = 0;

/// 简单的线性内存分配器
///
/// # Safety
/// 此函数仅应在单核环境下的早期启动阶段使用
pub unsafe fn alloc(layout: Layout) -> Option<usize> {
    let start = unsafe { RAM_CURRENT.align_up(layout.align()) };
    let end = start + layout.size();

    if end > unsafe { RAM_END } {
        return None;
    }

    unsafe {
        RAM_CURRENT = end;
    }
    Some(start)
}

pub unsafe fn alloc_and_flush_to_memory_map(layout: Layout, kind: MemoryType) -> Option<usize> {
    unsafe {
        let addr = alloc(layout)?;
        flush_to_memory_map(kind);
        Some(addr)
    }
}

pub unsafe fn flush_to_memory_map(kind: MemoryType) {
    let range = used_range();
    if range.is_empty() {
        return;
    }

    let end = range.end;
    let desc = MemoryDescriptor::new_with_range(range.clone(), kind);
    add_memory_descriptor(desc).unwrap();
    println!(
        "Flushed RAM used range to memory map: {:#x?}, current: {:#x}",
        range, end
    );
    unsafe {
        RAM_START = end;
        RAM_CURRENT = end;
    }
}

#[allow(dead_code)]
/// 获取当前分配位置
pub fn current() -> *mut u8 {
    unsafe { RAM_CURRENT as _ }
}

/// 初始化 RAM 分配器
pub fn init(range: Range<usize>) {
    println!("Initialize RAM allocator: {:#x?}", range);
    unsafe {
        RAM_START = range.start;
        RAM_END = range.end;
        RAM_CURRENT = range.start.max(0x40);
    }
}

/// 获取已使用的内存范围
pub fn used_range() -> Range<usize> {
    unsafe {
        let start = RAM_START;
        let end = RAM_CURRENT;
        start..end.align_up(page_size())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Ram;

impl FrameAllocator for Ram {
    fn alloc_frame(&self) -> Option<page_table_generic::PhysAddr> {
        unsafe {
            alloc(Layout::from_size_align_unchecked(page_size(), page_size())).map(|ptr| ptr.into())
        }
    }

    fn dealloc_frame(&self, _frame: page_table_generic::PhysAddr) {}

    fn phys_to_virt(&self, paddr: page_table_generic::PhysAddr) -> *mut u8 {
        super::phys_to_virt(paddr.raw())
    }
}

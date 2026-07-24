use core::{alloc::Layout, ops::Range};

use ax_kspin::SpinRaw;
use kernutil::memory::{MemoryDescriptor, MemoryType};
use num_align::NumAlign;
use page_table_generic::PageFrameProvider;

use crate::mem::{add_memory_descriptor, page_size};

#[derive(Clone, Copy)]
enum RamAllocatorState {
    Uninitialized,
    Active {
        used_start: usize,
        end: usize,
        current: usize,
    },
    Frozen,
}

// The early arena is used only by the BSP before secondary CPUs start. SpinRaw
// provides interior mutability without imposing runtime IRQ or scheduler hooks.
static RAM_ALLOCATOR: SpinRaw<RamAllocatorState> = SpinRaw::new(RamAllocatorState::Uninitialized);

/// Allocates from the early-boot linear RAM arena.
pub fn alloc(layout: Layout) -> Option<usize> {
    let mut allocator = RAM_ALLOCATOR.lock();
    let RamAllocatorState::Active {
        used_start,
        end: region_end,
        current,
    } = *allocator
    else {
        return None;
    };
    let align_mask = layout.align().checked_sub(1)?;
    let start = current.checked_add(align_mask)? & !align_mask;
    let end = start.checked_add(layout.size())?;
    if end > region_end {
        return None;
    }

    *allocator = RamAllocatorState::Active {
        used_start,
        end: region_end,
        current: end,
    };
    Some(start)
}

pub fn alloc_and_flush_to_memory_map(layout: Layout, kind: MemoryType) -> Option<usize> {
    let addr = alloc(layout)?;
    flush_to_memory_map(kind);
    Some(addr)
}

pub fn flush_to_memory_map(kind: MemoryType) {
    let mut allocator = RAM_ALLOCATOR.lock();
    let RamAllocatorState::Active {
        used_start,
        end: region_end,
        current,
    } = *allocator
    else {
        return;
    };
    let range = used_start..current.align_up(page_size());
    if range.is_empty() {
        return;
    }

    let end = range.end;
    let desc = MemoryDescriptor::new_with_range(range.clone(), kind);
    add_memory_descriptor(desc).expect("early RAM range must fit in the boot memory map");
    println!(
        "Flushed RAM used range to memory map: {:#x?}, current: {:#x}",
        range, end
    );
    *allocator = RamAllocatorState::Active {
        used_start: end,
        end: region_end,
        current: end,
    };
}

/// Initializes the early-boot RAM arena.
pub fn init(range: Range<usize>) {
    println!("Initialize RAM allocator: {:#x?}", range);
    *RAM_ALLOCATOR.lock() = RamAllocatorState::Active {
        used_start: range.start,
        end: range.end,
        current: range.start.max(0x40),
    };
}

/// Prevents further allocations before control is handed to the runtime.
pub fn freeze() {
    let mut allocator = RAM_ALLOCATOR.lock();
    if matches!(*allocator, RamAllocatorState::Active { .. }) {
        *allocator = RamAllocatorState::Frozen;
    }
}

/// Returns the active arena range not yet flushed to the memory map.
pub fn used_range() -> Range<usize> {
    match *RAM_ALLOCATOR.lock() {
        RamAllocatorState::Active {
            used_start,
            current,
            ..
        } => used_start..current.align_up(page_size()),
        RamAllocatorState::Uninitialized | RamAllocatorState::Frozen => 0..0,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Ram;

impl PageFrameProvider for Ram {
    fn alloc_frame(&self) -> Option<page_table_generic::PhysAddr> {
        self.alloc_frames(1, page_size())
    }

    fn dealloc_frame(&self, _paddr: page_table_generic::PhysAddr) {}

    fn alloc_frames(&self, count: usize, align: usize) -> Option<page_table_generic::PhysAddr> {
        let size = page_size().checked_mul(count)?;
        let layout = Layout::from_size_align(size, align).ok()?;
        alloc(layout).map(Into::into)
    }

    fn dealloc_frames(&self, _start: page_table_generic::PhysAddr, _count: usize) {}

    fn phys_to_virt(&self, paddr: page_table_generic::PhysAddr) -> page_table_generic::VirtAddr {
        page_table_generic::VirtAddr::from_usize(super::phys_to_virt(paddr.as_usize()) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_early_allocator_rejects_further_allocations() {
        init(0x1000..0x5000);
        let layout = Layout::from_size_align(0x1000, 0x1000).unwrap();
        assert_eq!(alloc(layout), Some(0x1000));

        freeze();

        assert_eq!(alloc(layout), None);
    }
}

//! Bitmap allocation in page-granularity.

use bitmap_allocator::BitAlloc;

use crate::{AllocError, AllocResult, BaseAllocator, PageAllocator};

const MAX_ALIGN_1GB: usize = 0x4000_0000;

cfg_if::cfg_if! {
    if #[cfg(test)] {
        /// Use 4GB memory for testing.
        type BitAllocUsed = bitmap_allocator::BitAlloc1M;
    } else if #[cfg(feature = "page-alloc-1t")] {
        /// Support max 256M * PAGE_SIZE = 1TB memory (assume that PAGE_SIZE = 4KB).
        type BitAllocUsed = bitmap_allocator::BitAlloc256M;
    } else if #[cfg(feature = "page-alloc-64g")] {
        /// Support max 16M * PAGE_SIZE = 64GB memory (assume that PAGE_SIZE = 4KB).
        type BitAllocUsed = bitmap_allocator::BitAlloc16M;
    } else if #[cfg(feature = "page-alloc-4g")] {
        /// Support max 1M * PAGE_SIZE = 4GB memory (assume that PAGE_SIZE = 4KB).
        type BitAllocUsed = bitmap_allocator::BitAlloc1M;
    } else {// #[cfg(feature = "page-alloc-256m")]
        /// Support max 64K * PAGE_SIZE = 256MB memory (assume that PAGE_SIZE = 4KB).
        type BitAllocUsed = bitmap_allocator::BitAlloc64K;
    }
}

/// A page-granularity memory allocator based on the [bitmap_allocator].
///
/// It internally uses a bitmap, each bit indicates whether a page has been
/// allocated.
///
/// The `PAGE_SIZE` must be a power of two.
pub struct BitmapPageAllocator<const PAGE_SIZE: usize> {
    base: usize,
    total_pages: usize,
    used_pages: usize,
    inner: BitAllocUsed,
}

impl<const PAGE_SIZE: usize> BitmapPageAllocator<PAGE_SIZE> {
    /// Creates a new empty `BitmapPageAllocator`.
    pub const fn new() -> Self {
        Self {
            base: 0,
            total_pages: 0,
            used_pages: 0,
            inner: BitAllocUsed::DEFAULT,
        }
    }
}

impl<const PAGE_SIZE: usize> BaseAllocator for BitmapPageAllocator<PAGE_SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        assert!(PAGE_SIZE.is_power_of_two());

        // Range for real:  [align_up(start, PAGE_SIZE), align_down(start + size, PAGE_SIZE))
        let end = crate::align_down(start + size, PAGE_SIZE);
        let start = crate::align_up(start, PAGE_SIZE);
        self.total_pages = (end - start) / PAGE_SIZE;

        // Calculate the base offset stored in the real [`BitAlloc`] instance.
        self.base = crate::align_down(start, MAX_ALIGN_1GB);

        // Range in bitmap: [start - self.base, start - self.base + total_pages * PAGE_SIZE)
        let start = start - self.base;
        let start_idx = start / PAGE_SIZE;

        self.inner.insert(start_idx..start_idx + self.total_pages);
    }

    fn add_memory(&mut self, _start: usize, _size: usize) -> AllocResult {
        Err(AllocError::NoMemory) // unsupported
    }
}

impl<const PAGE_SIZE: usize> PageAllocator for BitmapPageAllocator<PAGE_SIZE> {
    const PAGE_SIZE: usize = PAGE_SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        // Check if the alignment is valid.
        if align_pow2 > MAX_ALIGN_1GB || !crate::is_aligned(align_pow2, PAGE_SIZE) {
            return Err(AllocError::InvalidParam);
        }
        let align_pow2 = align_pow2 / PAGE_SIZE;
        if !align_pow2.is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }
        if num_pages > self.available_pages() {
            return Err(AllocError::NoMemory);
        }
        let align_log2 = align_pow2.trailing_zeros() as usize;
        match num_pages.cmp(&1) {
            core::cmp::Ordering::Equal => self.inner.alloc().map(|idx| idx * PAGE_SIZE + self.base),
            core::cmp::Ordering::Greater => self
                .inner
                .alloc_contiguous(None, num_pages, align_log2)
                .map(|idx| idx * PAGE_SIZE + self.base),
            _ => return Err(AllocError::InvalidParam),
        }
        .ok_or(AllocError::NoMemory)
        .inspect(|_| self.used_pages += num_pages)
    }

    /// Allocate pages at a specific address.
    fn alloc_pages_at(
        &mut self,
        base: usize,
        num_pages: usize,
        align_pow2: usize,
    ) -> AllocResult<usize> {
        // Check if the alignment is valid,
        // and the base address is aligned to the given alignment.
        if align_pow2 > MAX_ALIGN_1GB
            || !crate::is_aligned(align_pow2, PAGE_SIZE)
            || !crate::is_aligned(base, align_pow2)
        {
            return Err(AllocError::InvalidParam);
        }

        let align_pow2 = align_pow2 / PAGE_SIZE;
        if !align_pow2.is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }
        let align_log2 = align_pow2.trailing_zeros() as usize;

        let idx = (base - self.base) / PAGE_SIZE;

        self.inner
            .alloc_contiguous(Some(idx), num_pages, align_log2)
            .map(|idx| idx * PAGE_SIZE + self.base)
            .ok_or(AllocError::NoMemory)
            .inspect(|_| self.used_pages += num_pages)
    }

    fn dealloc_pages(&mut self, pos: usize, num_pages: usize) {
        assert!(
            crate::is_aligned(pos, Self::PAGE_SIZE),
            "pos must be aligned to PAGE_SIZE"
        );
        if match num_pages.cmp(&1) {
            core::cmp::Ordering::Equal => self.inner.dealloc((pos - self.base) / PAGE_SIZE),
            core::cmp::Ordering::Greater => self
                .inner
                .dealloc_contiguous((pos - self.base) / PAGE_SIZE, num_pages),
            _ => false,
        } {
            self.used_pages -= num_pages;
        }
    }

    fn total_pages(&self) -> usize {
        self.total_pages
    }

    fn used_pages(&self) -> usize {
        self.used_pages
    }

    fn available_pages(&self) -> usize {
        self.total_pages - self.used_pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE_SIZE: usize = 4096;

    #[test]
    fn test_bitmap_page_allocator_one_page() {
        let mut allocator = BitmapPageAllocator::<PAGE_SIZE>::new();
        allocator.init(PAGE_SIZE, PAGE_SIZE);

        assert_eq!(allocator.total_pages(), 1);
        assert_eq!(allocator.used_pages(), 0);
        assert_eq!(allocator.available_pages(), 1);

        let addr = allocator.alloc_pages(1, PAGE_SIZE).unwrap();
        assert_eq!(addr, 0x1000);
        assert_eq!(allocator.used_pages(), 1);
        assert_eq!(allocator.available_pages(), 0);

        allocator.dealloc_pages(addr, 1);
        assert_eq!(allocator.used_pages(), 0);
        assert_eq!(allocator.available_pages(), 1);

        let addr = allocator.alloc_pages(1, PAGE_SIZE).unwrap();
        assert_eq!(addr, 0x1000);
        assert_eq!(allocator.used_pages(), 1);
        assert_eq!(allocator.available_pages(), 0);
    }

    #[test]
    fn test_bitmap_page_allocator_size_2g() {
        const SIZE_1G: usize = 1024 * 1024 * 1024;
        const SIZE_2G: usize = 2 * SIZE_1G;

        const TEST_BASE_ADDR: usize = SIZE_1G + PAGE_SIZE;

        let mut allocator = BitmapPageAllocator::<PAGE_SIZE>::new();
        allocator.init(TEST_BASE_ADDR, SIZE_2G);

        let mut num_pages = 1;
        // Test allocation and deallocation of 1, 10, 100, 1000 pages.
        while num_pages <= 1000 {
            assert_eq!(allocator.total_pages(), SIZE_2G / PAGE_SIZE);
            assert_eq!(allocator.used_pages(), 0);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE);

            let addr = allocator.alloc_pages(num_pages, PAGE_SIZE).unwrap();
            assert_eq!(addr, TEST_BASE_ADDR);
            assert_eq!(allocator.used_pages(), num_pages);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE - num_pages);

            allocator.dealloc_pages(addr, num_pages);
            assert_eq!(allocator.used_pages(), 0);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE);

            num_pages *= 10;
        }

        // Test allocation and deallocation of 1, 10, 100 pages with alignment.
        num_pages = 1;
        let mut align = PAGE_SIZE;
        while align <= MAX_ALIGN_1GB {
            assert_eq!(allocator.total_pages(), SIZE_2G / PAGE_SIZE);
            assert_eq!(allocator.used_pages(), 0);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE);

            let addr = allocator.alloc_pages(num_pages, align).unwrap();
            assert_eq!(addr, crate::align_up(TEST_BASE_ADDR, align));
            assert_eq!(allocator.used_pages(), num_pages);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE - num_pages);

            allocator.dealloc_pages(addr, num_pages);
            assert_eq!(allocator.used_pages(), 0);
            assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE);

            num_pages *= 10;
            align <<= 9;
        }

        num_pages = 1;
        align = PAGE_SIZE;
        let mut i = 0;
        let mut addrs = [(0, 0); 3];
        let mut used_pages = 0;

        // Test allocation of 1, 10, 100 pages with alignment.
        while i < 3 {
            assert_eq!(allocator.total_pages(), SIZE_2G / PAGE_SIZE);
            assert_eq!(allocator.used_pages(), used_pages);
            assert_eq!(
                allocator.available_pages(),
                SIZE_2G / PAGE_SIZE - used_pages
            );

            let addr = allocator.alloc_pages(num_pages, align).unwrap();
            assert!(crate::is_aligned(addr, align));

            addrs[i] = (addr, num_pages);

            used_pages += num_pages;
            assert_eq!(allocator.used_pages(), used_pages);
            assert_eq!(
                allocator.available_pages(),
                SIZE_2G / PAGE_SIZE - used_pages
            );

            num_pages *= 10;
            align <<= 9;
            i += 1;
        }

        i = 0;
        // Test deallocation of 1, 10, 100 pages.
        while i < 3 {
            let addr = addrs[i].0;
            let num_pages = addrs[i].1;
            allocator.dealloc_pages(addr, num_pages);

            used_pages -= num_pages;
            assert_eq!(allocator.used_pages(), used_pages);
            assert_eq!(
                allocator.available_pages(),
                SIZE_2G / PAGE_SIZE - used_pages
            );
            i += 1;
        }

        assert_eq!(allocator.used_pages(), 0);
        assert_eq!(allocator.available_pages(), SIZE_2G / PAGE_SIZE);

        // Test allocation of 1, 10, 100 pages with alignment at a specific address.
        num_pages = 1;
        align = PAGE_SIZE;
        i = 0;
        used_pages = 0;
        let mut test_addr_base = TEST_BASE_ADDR;

        while i < 3 {
            assert_eq!(allocator.total_pages(), SIZE_2G / PAGE_SIZE);
            assert_eq!(allocator.used_pages(), used_pages);
            assert_eq!(
                allocator.available_pages(),
                SIZE_2G / PAGE_SIZE - used_pages
            );

            let addr = allocator
                .alloc_pages_at(test_addr_base, num_pages, align)
                .unwrap();
            assert_eq!(addr, test_addr_base);

            used_pages += num_pages;
            assert_eq!(allocator.used_pages(), used_pages);
            assert_eq!(
                allocator.available_pages(),
                SIZE_2G / PAGE_SIZE - used_pages
            );

            num_pages *= 10;
            align <<= 9;

            test_addr_base = crate::align_up(test_addr_base + num_pages * PAGE_SIZE, align);

            i += 1;
        }
    }
}

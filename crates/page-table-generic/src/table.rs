use crate::{
    FramAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, TableGeneric, VirtAddr,
};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MapConfig<P: PageTableEntry> {
    pub vaddr: VirtAddr,
    pub paddr: PhysAddr,
    pub size: usize,
    pub pte: P,
    pub allow_huge: bool,
    pub flush: bool,
}

pub struct PageTable<T: TableGeneric, A: FramAllocator> {
    root: Frame<T, A>,
}

impl<T: TableGeneric, A: FramAllocator> PageTable<T, A> {
    /// 创建一个新的页表
    pub fn new(allocator: A) -> PagingResult<Self> {
        let root = Frame::new(allocator)?;
        Ok(Self { root })
    }

    pub fn map(&mut self, config: &MapConfig<T::P>) -> PagingResult {
        Ok(())
    }
}

struct Frame<T: TableGeneric, A: FramAllocator> {
    paddr: PhysAddr,
    allocator: A,
    _marker: core::marker::PhantomData<T>,
}

impl<T, A> Frame<T, A>
where
    T: TableGeneric,
    A: FramAllocator,
{
    fn new(allocator: A) -> PagingResult<Self> {
        let paddr = allocator.alloc_frame().ok_or(PagingError::NoMemory)?;
        unsafe {
            let vaddr = allocator.phys_to_virt(paddr);
            core::ptr::write_bytes(vaddr, 0, T::PAGE_SIZE);
        }

        Ok(Self {
            paddr,
            allocator,
            _marker: core::marker::PhantomData,
        })
    }

    fn as_slice_mut(&mut self) -> &mut [T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts_mut(vaddr as *mut T::P, T::TABLE_LEN) }
    }

    fn as_slice(&self) -> &[T::P] {
        let vaddr = self.allocator.phys_to_virt(self.paddr);
        unsafe { core::slice::from_raw_parts(vaddr as *const T::P, T::TABLE_LEN) }
    }
}

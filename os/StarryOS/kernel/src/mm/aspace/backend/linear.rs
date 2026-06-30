use alloc::sync::Arc;

use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageSize, PageTableCursor, PagingError};
use ax_sync::Mutex;

use super::{AddrSpace, Backend, BackendOps, CloneMapAccounting, MemoryAccounting, pages_in};

/// Linear mapping backend.
///
/// The offset between the virtual address and the physical address is
/// constant, which is specified by `pa_va_offset`. For example, the virtual
/// address `vaddr` is mapped to the physical address `vaddr - pa_va_offset`.
///
/// Device/DMA and signal-trampoline mappings use this backend; they are not
/// counted in process RSS (Linux `VM_PFNMAP|VM_IO` analogue).
#[derive(Clone)]
pub struct LinearBackend {
    start: VirtAddr,
    offset: isize,
    shared: bool,
    /// Optional lifetime anchor. Keeps an arbitrary object alive as long as
    /// this backend (and its VMA) exists. Used, for example, to keep an
    /// `Arc<IonBuffer>` alive while its physical DMA pages are mapped into a
    /// process address space, preventing use-after-free when the fd is closed
    /// before `munmap`.
    anchor: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

impl LinearBackend {
    pub fn with_start(&self, new_start: VirtAddr) -> Self {
        Self {
            start: new_start,
            offset: self.offset + (new_start.as_usize() as isize - self.start.as_usize() as isize),
            shared: self.shared,
            anchor: self.anchor.clone(),
        }
    }

    fn pa(&self, va: VirtAddr) -> PhysAddr {
        PhysAddr::from((va.as_usize() as isize - self.offset) as usize)
    }

    pub const fn is_shared(&self) -> bool {
        self.shared
    }
}

impl BackendOps for LinearBackend {
    fn page_size(&self) -> PageSize {
        PageSize::Size4K
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        _acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let pa_range =
            ax_memory_addr::PhysAddrRange::from_start_size(self.pa(range.start), range.size());
        debug!("Linear::map: {range:?} -> {pa_range:?} {flags:?}");
        pt.map_region(range.start, |va| self.pa(va), range.size(), flags, false)?;
        Ok(())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        _acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        let pa_range =
            ax_memory_addr::PhysAddrRange::from_start_size(self.pa(range.start), range.size());
        debug!("Linear::unmap: {range:?} -> {pa_range:?}");
        for vaddr in pages_in(range, PageSize::Size4K)? {
            match pt.unmap(vaddr) {
                Ok((_, _, page_size)) => debug_assert_eq!(page_size, PageSize::Size4K),
                Err(PagingError::NotMapped) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn clone_map(
        &self,
        _range: VirtAddrRange,
        _flags: MappingFlags,
        _old_pt: &mut PageTableCursor,
        _new_pt: &mut PageTableCursor,
        _new_aspace: &Arc<Mutex<AddrSpace>>,
        _acct: CloneMapAccounting<'_>,
    ) -> AxResult<Backend> {
        Ok(Backend::Linear(self.clone()))
    }

    fn split(&mut self, _align_diff: usize) -> Option<Backend> {
        Some(Backend::Linear(self.clone()))
    }

    fn shrink_left(&mut self, _shrink_size: usize) {}

    fn shrink_right(&mut self, _shrink_size: usize) {}
}

impl Backend {
    pub fn new_linear(start: VirtAddr, offset: isize, shared: bool) -> Self {
        Self::Linear(LinearBackend {
            start,
            offset,
            shared,
            anchor: None,
        })
    }

    pub fn new_linear_anchored(
        start: VirtAddr,
        offset: isize,
        shared: bool,
        anchor: Arc<dyn core::any::Any + Send + Sync>,
    ) -> Self {
        Self::Linear(LinearBackend {
            start,
            offset,
            shared,
            anchor: Some(anchor),
        })
    }
}

//! Memory mapping backends.
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};

use ax_alloc::{UsageKind, global_allocator};
use ax_errno::{AxError, AxResult};
use ax_memory_addr::{DynPageIter, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_memory_set::MappingBackend;
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageSize, PageTable, PageTableCursor},
};
use ax_sync::Mutex;
use enum_dispatch::enum_dispatch;

mod cow;
mod file;
mod linear;
mod shared;

use starry_mm::{CommitKind, MemoryAccounting, PageInitialization, PageSource};
pub use starry_mm::{RssKind, SharedPages};

#[cfg(axtest)]
pub(crate) use self::cow::fault_accounting_failure_rolls_back_for_test;
use super::AddrSpace;

fn divide_page(size: usize, page_size: PageSize) -> usize {
    assert!(page_size.is_aligned(size), "unaligned");
    size >> (page_size as usize).trailing_zeros()
}

pub(crate) fn alloc_frame(
    initialization: PageInitialization,
    size: PageSize,
) -> AxResult<PhysAddr> {
    let page_size = size as usize;
    let num_pages = page_size / PAGE_SIZE_4K;
    let vaddr = VirtAddr::from(
        global_allocator()
            .alloc_pages(
                ax_alloc::PageRequest {
                    count: num_pages,
                    align: page_size,
                    zone: ax_alloc::MemoryZone::Normal,
                },
                UsageKind::VirtMem,
            )
            .map_err(|_| AxError::NoMemory)?,
    );
    if initialization == PageInitialization::Zeroed {
        // SAFETY: the allocator returned exclusive ownership of `page_size`
        // writable bytes beginning at `vaddr`.
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, page_size) };
    }
    let paddr = virt_to_phys(vaddr);

    Ok(paddr)
}

pub(crate) fn dealloc_frame(frame: PhysAddr, align: PageSize) {
    let vaddr = phys_to_virt(frame);
    let page_size: usize = align.into();
    let num_pages = page_size / PAGE_SIZE_4K;
    // SAFETY: VM backends transfer only exclusive frames returned by
    // alloc_frame and preserve their page-size-derived request metadata.
    unsafe {
        global_allocator().dealloc_pages(vaddr.as_usize(), num_pages, UsageKind::VirtMem);
    }
}

struct RuntimePageSource;

impl PageSource for RuntimePageSource {
    fn alloc_page(&self, initialization: PageInitialization, size: PageSize) -> AxResult<PhysAddr> {
        alloc_frame(initialization, size)
    }

    fn dealloc_page(&self, paddr: PhysAddr, size: PageSize) {
        dealloc_frame(paddr, size);
    }
}

static RUNTIME_PAGE_SOURCE: RuntimePageSource = RuntimePageSource;

pub(crate) const fn runtime_page_source() -> &'static dyn PageSource {
    &RUNTIME_PAGE_SOURCE
}

pub(super) fn pages_in(range: VirtAddrRange, align: PageSize) -> AxResult<DynPageIter<VirtAddr>> {
    DynPageIter::new(range.start, range.end, align as usize).ok_or(AxError::InvalidInput)
}

pub(super) type PopulateCallback = Box<dyn FnOnce(&mut AddrSpace)>;

/// Page-table and resident accounting state updated together by Starry mapping backends.
#[doc(hidden)]
pub struct AddressSpacePageTable {
    pub(super) table: PageTable,
    pub(super) accounting: MemoryAccounting,
}

impl AddressSpacePageTable {
    pub(super) fn try_new() -> AxResult<Self> {
        Ok(Self {
            table: PageTable::try_new().map_err(|_| AxError::NoMemory)?,
            accounting: MemoryAccounting::new(),
        })
    }
}

#[enum_dispatch]
pub trait BackendOps {
    /// Returns the page size of the backend.
    fn page_size(&self) -> PageSize;

    /// Map a memory region.
    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult;

    /// Unmap a memory region.
    fn unmap(
        &self,
        range: VirtAddrRange,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult;

    /// Called before a memory region is protected.
    fn on_protect(
        &self,
        _range: VirtAddrRange,
        _new_flags: MappingFlags,
        _pt: &mut PageTableCursor,
    ) -> AxResult {
        Ok(())
    }

    /// Populate a memory region and return how many pages now satisfy
    /// `access_flags`.
    ///
    /// If another thread has already mapped the page with sufficient permissions,
    /// treat it as populated.
    fn populate(
        &self,
        _range: VirtAddrRange,
        _flags: MappingFlags,
        _access_flags: MappingFlags,
        _acct: Option<&MemoryAccounting>,
        _pt: &mut PageTableCursor,
    ) -> AxResult<(usize, Option<PopulateCallback>)> {
        Ok((0, None))
    }

    /// Duplicates this mapping for use in a different page table.
    ///
    /// This differs from `clone`, which is designed for splitting a mapping
    /// within the same table.
    ///
    /// [`BackendOps::map`] will be latter called to the returned backend.
    fn clone_map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        old_pt: &mut PageTableCursor,
        new_pt: &mut PageTableCursor,
        new_aspace: &Arc<Mutex<AddrSpace>>,
        child_accounting: Option<&MemoryAccounting>,
    ) -> AxResult<Backend>;

    /// Splits the backend into two at the given position, and returns the backend for the upper part.
    ///
    /// The original backend is shrunk to the lower part.
    ///
    /// Returns `None` if the given position is not in the memory area, or one
    /// of the parts is empty after splitting.
    fn split(&mut self, align_diff: usize) -> Option<Backend>;
}

/// A unified enum type for different memory mapping backends.
#[derive(Clone)]
#[enum_dispatch(BackendOps)]
pub enum Backend {
    Linear(linear::LinearBackend),
    Cow(cow::CowBackend),
    Shared(shared::SharedBackend),
    File(file::FileBackend),
}

pub struct BackendFileInfo {
    pub path: String,
    pub offset: Option<u64>,
    pub inode: Option<u64>,
    pub dev: Option<u64>,
    pub shared: bool,
}

impl Backend {
    /// Returns the committed bytes represented by this backend and VMA flags.
    pub fn accounted_bytes(&self, flags: MappingFlags, bytes: usize) -> u64 {
        let kind = match self {
            Self::Linear(_) | Self::File(_) => CommitKind::Unaccounted,
            Self::Cow(backend) if backend.is_anonymous() => CommitKind::PrivateAnonymous,
            Self::Cow(_) => CommitKind::PrivateFile,
            Self::Shared(_) => CommitKind::Unaccounted,
        };
        kind.accounted_bytes(flags.contains(MappingFlags::WRITE), bytes as u64)
    }

    /// Returns the file information if this is a file-backed mapping, or `None` otherwise.
    ///
    /// The returned tuple contains the file name, offset, inode and whether the mapping is shared.
    pub fn file_info(&self) -> AxResult<BackendFileInfo> {
        match self {
            Backend::Cow(b) => b.file_info(),
            Backend::Linear(b) => Ok(BackendFileInfo {
                path: "".to_string(),
                offset: None,
                inode: None,
                dev: None,
                shared: b.is_shared(),
            }),
            Backend::Shared(_) => Ok(BackendFileInfo {
                path: "".to_string(),
                offset: None,
                inode: None,
                dev: None,
                shared: true,
            }),
            Backend::File(b) => b.file_info(),
        }
    }

    /// Clone with a different base address (for mremap moves).
    /// `src_offset` is the distance from the original VMA start to the
    /// mremap source address, used to adjust file/page offsets.
    pub fn relocated(
        &self,
        new_start: VirtAddr,
        src_offset: usize,
        aspace: &Arc<Mutex<AddrSpace>>,
    ) -> AxResult<Self> {
        let adjusted = new_start
            .as_usize()
            .checked_sub(src_offset)
            .map(VirtAddr::from)
            .ok_or(AxError::InvalidInput)?;
        Ok(match self {
            Self::Cow(cb) => Self::Cow(cb.with_start(adjusted)),
            Self::Shared(sb) => Self::Shared(sb.with_start(adjusted)),
            Self::Linear(_) => return Err(AxError::OperationNotSupported),
            Self::File(fb) => Self::File(fb.with_start(adjusted, aspace)),
        })
    }
}

impl MappingBackend for Backend {
    type Addr = VirtAddr;
    type Flags = MappingFlags;
    type PageTable = AddressSpacePageTable;

    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut AddressSpacePageTable,
    ) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        if let Err(error) = BackendOps::map(
            self,
            range,
            flags,
            Some(&pt.accounting),
            &mut pt.table.cursor(),
        ) {
            warn!("Failed to map area: {error:?}");
            false
        } else {
            true
        }
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut AddressSpacePageTable) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        if let Err(error) =
            BackendOps::unmap(self, range, Some(&pt.accounting), &mut pt.table.cursor())
        {
            warn!("Failed to unmap area: {error:?}");
            false
        } else {
            true
        }
    }

    fn protect(
        &self,
        start: VirtAddr,
        size: usize,
        new_flags: MappingFlags,
        pt: &mut AddressSpacePageTable,
    ) -> bool {
        let range = VirtAddrRange::from_start_size(start, size);
        let mut cursor = pt.table.cursor();
        if let Err(error) = BackendOps::on_protect(self, range, new_flags, &mut cursor) {
            warn!("Failed to protect area: {error:?}");
            return false;
        }
        let pte_flags = match self {
            Backend::Cow(cow) => cow.pte_flags_for_protect(new_flags),
            _ => new_flags,
        };
        cursor.protect_region(start, size, pte_flags).is_ok()
    }

    fn split(&mut self, align_diff: usize) -> Option<Self> {
        BackendOps::split(self, align_diff)
    }
}

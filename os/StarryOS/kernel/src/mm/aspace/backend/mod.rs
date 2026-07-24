//! Memory mapping backends.
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use ax_alloc::{UsageKind, global_allocator};
use ax_errno::{AxError, AxResult};
use ax_memory_addr::{DynPageIter, MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};
use ax_memory_set::{
    MapPrecondition, MappingBackend, MappingError, MappingOperation, MappingResult,
};
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::{MappingFlags, PageSize, PageTable, PageTableCursor, PagingError},
};
use ax_sync::Mutex;
use enum_dispatch::enum_dispatch;

mod cow;
mod file;
mod linear;
mod shared;

use starry_mm::{CloneMapAccounting, CommitKind, MemoryAccounting, PageInitialization, PageSource};
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
            .allocate_pages_raw(
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
        global_allocator().deallocate_pages_raw(
            vaddr.as_usize(),
            ax_alloc::PageRelease {
                count: num_pages,
                zone: ax_alloc::MemoryZone::Normal,
            },
            UsageKind::VirtMem,
        );
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
        acct: CloneMapAccounting<'_>,
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
    #[inline(never)]
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

#[derive(Clone, Copy)]
struct SavedMapping {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    flags: MappingFlags,
    page_size: PageSize,
    rss_kind: Option<RssKind>,
    cow_hold: bool,
}

#[doc(hidden)]
pub struct BackendTransaction {
    operation: MappingOperation<VirtAddr, MappingFlags>,
    previous: Vec<SavedMapping>,
}

impl MappingBackend for Backend {
    type Addr = VirtAddr;
    type Flags = MappingFlags;
    type PageTable = PageTable;
    type MappingPlan = BackendTransaction;
    type CommitState = BackendTransaction;

    fn prepare(
        &self,
        operation: MappingOperation<VirtAddr, MappingFlags>,
        pt: &mut PageTable,
    ) -> MappingResult<Self::MappingPlan> {
        self.validate_operation(operation)?;
        let (start, size) = operation.range();
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let range = VirtAddrRange::new(start, end);
        let page_size = self.page_size();
        let mut previous = Vec::new();
        let mut contains_unmapped = false;
        starry_mm::with_rss_accounting(|acct| {
            let pages = pages_in(range, page_size).map_err(map_ax_error)?;
            for vaddr in pages {
                match pt.cursor().query(vaddr) {
                    Ok((paddr, flags, actual_size)) if actual_size == page_size => {
                        if matches!(
                            operation,
                            MappingOperation::Map {
                                precondition: MapPrecondition::Vacant,
                                ..
                            }
                        ) {
                            self.release_holds(&previous);
                            return Err(MappingError::AlreadyExists);
                        }
                        if !matches!(operation, MappingOperation::Map { .. })
                            && previous.try_reserve(1).is_err()
                        {
                            self.release_holds(&previous);
                            return Err(MappingError::NoMemory);
                        }
                        let cow_hold = matches!(operation, MappingOperation::Unmap { .. })
                            && matches!(self, Self::Cow(_));
                        if cow_hold && let Err(error) = cow::retain_frame_for_transaction(paddr) {
                            self.release_holds(&previous);
                            return Err(map_ax_error(error));
                        }
                        if !matches!(operation, MappingOperation::Map { .. }) {
                            previous.push(SavedMapping {
                                vaddr,
                                paddr,
                                flags,
                                page_size: actual_size,
                                rss_kind: self.rss_kind_before_unmap(vaddr, acct),
                                cow_hold,
                            });
                        }
                    }
                    Ok(_) => {
                        self.release_holds(&previous);
                        return Err(MappingError::BadState);
                    }
                    Err(PagingError::NotMapped) => contains_unmapped = true,
                    Err(_) => {
                        self.release_holds(&previous);
                        return Err(MappingError::BadState);
                    }
                }
            }

            if matches!(self, Self::Linear { .. })
                && matches!(operation, MappingOperation::Unmap { .. })
                && contains_unmapped
            {
                self.release_holds(&previous);
                return Err(MappingError::BadState);
            }
            Ok(BackendTransaction {
                operation,
                previous,
            })
        })
    }

    fn abort(&self, plan: Self::MappingPlan, _pt: &mut PageTable) {
        self.release_holds(&plan.previous);
    }

    fn commit(
        &self,
        plan: Self::MappingPlan,
        pt: &mut PageTable,
    ) -> MappingResult<Self::CommitState> {
        match self.apply(plan.operation, pt) {
            Ok(()) => Ok(plan),
            Err(error) => {
                warn!("Failed to commit memory mapping operation: {error:?}");
                let original = map_ax_error(error);
                self.restore(plan, pt).map_err(|restore_error| {
                    warn!("Failed to restore a partially committed operation: {restore_error:?}");
                    MappingError::BadState
                })?;
                Err(original)
            }
        }
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut Self::PageTable) -> MappingResult {
        self.restore(state, pt).map_err(|error| {
            warn!("Failed to roll back memory mapping operation: {error:?}");
            MappingError::BadState
        })
    }

    fn finalize(&self, state: Self::CommitState, _pt: &mut PageTable) {
        self.release_holds(&state.previous);
    }

    fn split(&mut self, align_diff: usize) -> Option<Self> {
        BackendOps::split(self, align_diff)
    }
}

impl Backend {
    fn validate_operation(
        &self,
        operation: MappingOperation<VirtAddr, MappingFlags>,
    ) -> MappingResult {
        let (start, size) = operation.range();
        let page_size = usize::from(self.page_size());
        start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        if !start.as_usize().is_multiple_of(page_size) || !size.is_multiple_of(page_size) {
            return Err(MappingError::InvalidParam);
        }
        let requested_flags = match operation {
            MappingOperation::Map { flags, .. } => Some(flags),
            MappingOperation::Protect { new_flags, .. } => Some(new_flags),
            MappingOperation::Unmap { .. } => None,
        };
        if let (Self::File(file), Some(flags)) = (self, requested_flags) {
            file.check_flags(flags).map_err(map_ax_error)?;
        }
        Ok(())
    }

    fn rss_kind_before_unmap(
        &self,
        vaddr: VirtAddr,
        acct: Option<&MemoryAccounting>,
    ) -> Option<RssKind> {
        let acct = acct?;
        match self {
            Self::Cow(_) => acct.charge_kind(vaddr),
            Self::Shared(_) => Some(RssKind::Shmem),
            Self::File(file) => Some(file.rss_kind()),
            Self::Linear(_) => None,
        }
    }

    fn release_holds(&self, mappings: &[SavedMapping]) {
        for saved in mappings.iter().filter(|saved| saved.cow_hold) {
            cow::release_transaction_frame(saved.paddr, saved.page_size);
        }
    }

    fn restore(&self, state: BackendTransaction, pt: &mut PageTable) -> AxResult {
        starry_mm::with_rss_accounting(|acct| {
            match state.operation {
                MappingOperation::Map { start, size, .. } => {
                    let range = VirtAddrRange::from_start_size(start, size);
                    BackendOps::unmap(self, range, acct, &mut pt.cursor())?;
                }
                MappingOperation::Unmap { .. } => {
                    for saved in state.previous {
                        let current = {
                            let cursor = pt.cursor();
                            cursor.query(saved.vaddr)
                        };
                        match current {
                            Ok((paddr, flags, page_size))
                                if paddr == saved.paddr
                                    && flags == saved.flags
                                    && page_size == saved.page_size =>
                            {
                                if saved.cow_hold {
                                    cow::release_transaction_frame(saved.paddr, saved.page_size);
                                }
                            }
                            Err(PagingError::NotMapped) => {
                                pt.cursor().map(
                                    saved.vaddr,
                                    saved.paddr,
                                    saved.page_size,
                                    saved.flags,
                                )?;
                                if let (Some(acct), Some(kind)) = (acct, saved.rss_kind) {
                                    if matches!(self, Self::Cow(_)) {
                                        acct.record_charge(saved.vaddr, kind)?;
                                    } else {
                                        acct.inc(kind, 1);
                                    }
                                }
                                // A retained COW reference now belongs to the
                                // restored mapping and must not be released.
                            }
                            _ => return Err(AxError::BadState),
                        }
                    }
                }
                MappingOperation::Protect { .. } => {
                    for saved in state.previous {
                        let restored = {
                            let mut cursor = pt.cursor();
                            cursor.protect(saved.vaddr, saved.flags)
                        };
                        match restored {
                            Ok(page_size) if page_size == saved.page_size => {}
                            Err(PagingError::NotMapped) => {
                                pt.cursor().map(
                                    saved.vaddr,
                                    saved.paddr,
                                    saved.page_size,
                                    saved.flags,
                                )?;
                            }
                            _ => return Err(AxError::BadState),
                        }
                    }
                }
            }
            Ok(())
        })
    }

    fn apply(
        &self,
        operation: MappingOperation<VirtAddr, MappingFlags>,
        pt: &mut PageTable,
    ) -> AxResult {
        match operation {
            MappingOperation::Map {
                start, size, flags, ..
            } => {
                let range = VirtAddrRange::from_start_size(start, size);
                starry_mm::with_rss_accounting(|acct| {
                    BackendOps::map(self, range, flags, acct, &mut pt.cursor())
                })
            }
            MappingOperation::Unmap { start, size, .. } => {
                let range = VirtAddrRange::from_start_size(start, size);
                starry_mm::with_rss_accounting(|acct| {
                    BackendOps::unmap(self, range, acct, &mut pt.cursor())
                })
            }
            MappingOperation::Protect {
                start,
                size,
                new_flags,
                ..
            } => {
                let range = VirtAddrRange::from_start_size(start, size);
                let mut cursor = pt.cursor();
                BackendOps::on_protect(self, range, new_flags, &mut cursor)?;
                let pte_flags = match self {
                    Backend::Cow(c) => c.pte_flags_for_protect(new_flags),
                    _ => new_flags,
                };
                cursor.protect_region(start, size, pte_flags)?;
                Ok(())
            }
        }
    }
}

fn map_ax_error(error: AxError) -> MappingError {
    match error {
        AxError::NoMemory => MappingError::NoMemory,
        AxError::InvalidInput => MappingError::InvalidParam,
        _ => MappingError::BadState,
    }
}

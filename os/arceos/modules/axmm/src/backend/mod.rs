//! Memory mapping backends.

use ::alloc::vec::Vec;
use ax_hal::paging::{MappingFlags, PageSize, PageTable, PagingError};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr, VirtAddr};
use ax_memory_set::{
    MapPrecondition, MappingBackend, MappingError, MappingOperation, MappingResult,
};

mod alloc;
mod linear;

use self::alloc::dealloc_frame;

/// A unified enum type for different memory mapping backends.
///
/// Currently, two backends are implemented:
///
/// - **Linear**: used for linear mappings. The target physical frames are
///   contiguous and their addresses should be known when creating the mapping.
/// - **Allocation**: used in general, or for lazy mappings. The target physical
///   frames are obtained from the global allocator.
#[derive(Clone)]
pub enum Backend {
    /// Linear mapping backend.
    ///
    /// The offset between the virtual address and the physical address is
    /// constant, which is specified by `pa_va_offset`. For example, the virtual
    /// address `vaddr` is mapped to the physical address `vaddr - pa_va_offset`.
    Linear {
        /// `vaddr - paddr`.
        pa_va_offset: i128,
    },
    /// Allocation mapping backend.
    ///
    /// If `populate` is `true`, all physical frames are allocated when the
    /// mapping is created, and no page faults are triggered during the memory
    /// access. Otherwise, the physical frames are allocated on demand (by
    /// handling page faults).
    Alloc {
        /// Whether to populate the physical frames when creating the mapping.
        populate: bool,
    },
}

#[derive(Clone, Copy)]
struct SavedMapping {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    flags: MappingFlags,
}

#[doc(hidden)]
pub struct BackendTransaction {
    operation: MappingOperation<VirtAddr, MappingFlags>,
    previous: Vec<Option<SavedMapping>>,
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
        let (start, size) = operation.range();
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let pages = PageIter4K::new(start, end).ok_or(MappingError::InvalidParam)?;
        let mut previous = Vec::new();
        previous
            .try_reserve_exact(size / PAGE_SIZE_4K)
            .map_err(|_| MappingError::NoMemory)?;
        for vaddr in pages {
            match pt.query(vaddr) {
                Ok((paddr, flags, PageSize::Size4K)) => previous.push(Some(SavedMapping {
                    vaddr,
                    paddr,
                    flags,
                })),
                Ok((..)) => return Err(MappingError::BadState),
                Err(PagingError::NotMapped) => previous.push(None),
                Err(_) => return Err(MappingError::BadState),
            }
        }

        if matches!(
            operation,
            MappingOperation::Map {
                precondition: MapPrecondition::Vacant,
                ..
            }
        ) && previous.iter().any(Option::is_some)
        {
            return Err(MappingError::AlreadyExists);
        }
        if let (Self::Linear { pa_va_offset }, MappingOperation::Map { start, size, .. }) =
            (self, operation)
        {
            let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
            if Self::linear_paddr(start, *pa_va_offset).is_none()
                || Self::linear_paddr(end, *pa_va_offset).is_none()
            {
                return Err(MappingError::InvalidParam);
            }
        }
        if matches!(self, Self::Linear { .. })
            && matches!(operation, MappingOperation::Unmap { .. })
            && previous.iter().any(Option::is_none)
        {
            return Err(MappingError::BadState);
        }
        Ok(BackendTransaction {
            operation,
            previous,
        })
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut PageTable) {}

    fn commit(
        &self,
        plan: Self::MappingPlan,
        pt: &mut PageTable,
    ) -> MappingResult<Self::CommitState> {
        if self.apply(plan.operation, pt) {
            Ok(plan)
        } else {
            self.rollback(plan, pt)?;
            Err(MappingError::BadState)
        }
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut PageTable) -> MappingResult {
        match state.operation {
            MappingOperation::Map { .. } => {
                let (start, size) = state.operation.range();
                let end = start.checked_add(size).ok_or(MappingError::BadState)?;
                for (vaddr, previous) in PageIter4K::new(start, end)
                    .ok_or(MappingError::BadState)?
                    .zip(state.previous)
                {
                    match pt.cursor().unmap(vaddr) {
                        Ok((frame, _, PageSize::Size4K)) => {
                            if matches!(self, Self::Alloc { .. }) {
                                dealloc_frame(frame);
                            }
                        }
                        Ok(_) | Err(PagingError::NotMapped) => {}
                        Err(_) => return Err(MappingError::BadState),
                    }
                    if let Some(saved) = previous {
                        pt.cursor()
                            .map(saved.vaddr, saved.paddr, PageSize::Size4K, saved.flags)
                            .map_err(|_| MappingError::BadState)?;
                    }
                }
            }
            MappingOperation::Unmap { .. } => {
                for saved in state.previous.into_iter().flatten() {
                    match pt.query(saved.vaddr) {
                        Err(PagingError::NotMapped) => pt
                            .cursor()
                            .map(saved.vaddr, saved.paddr, PageSize::Size4K, saved.flags)
                            .map_err(|_| MappingError::BadState)?,
                        Ok((paddr, flags, PageSize::Size4K))
                            if paddr == saved.paddr && flags == saved.flags => {}
                        _ => return Err(MappingError::BadState),
                    }
                }
            }
            MappingOperation::Protect { .. } => {
                for saved in state.previous.into_iter().flatten() {
                    pt.cursor()
                        .protect(saved.vaddr, saved.flags)
                        .map_err(|_| MappingError::BadState)?;
                }
            }
        }
        Ok(())
    }

    fn finalize(&self, state: Self::CommitState, _pt: &mut PageTable) {
        if matches!(self, Self::Alloc { .. })
            && matches!(state.operation, MappingOperation::Unmap { .. })
        {
            for saved in state.previous.into_iter().flatten() {
                dealloc_frame(saved.paddr);
            }
        }
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        // backend can be trivially split since it does not have any state.
        Some(self.clone())
    }
}

impl Backend {
    fn apply(
        &self,
        operation: MappingOperation<VirtAddr, MappingFlags>,
        page_table: &mut PageTable,
    ) -> bool {
        match operation {
            MappingOperation::Map {
                start, size, flags, ..
            } => match *self {
                Self::Linear { pa_va_offset } => {
                    self.map_linear(start, size, flags, page_table, pa_va_offset)
                }
                Self::Alloc { populate } => {
                    self.map_alloc(start, size, flags, page_table, populate)
                }
            },
            MappingOperation::Unmap { start, size, .. } => match *self {
                Self::Linear { pa_va_offset } => {
                    self.unmap_linear(start, size, page_table, pa_va_offset)
                }
                Self::Alloc { populate } => self.unmap_alloc(start, size, page_table, populate),
            },
            MappingOperation::Protect {
                start,
                size,
                new_flags,
                ..
            } => page_table
                .cursor()
                .protect_region(start, size, new_flags)
                .is_ok(),
        }
    }

    pub(crate) fn handle_page_fault(
        &self,
        vaddr: VirtAddr,
        orig_flags: MappingFlags,
        page_table: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { .. } => false, // Linear mappings should not trigger page faults.
            Self::Alloc { populate } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
        }
    }
}

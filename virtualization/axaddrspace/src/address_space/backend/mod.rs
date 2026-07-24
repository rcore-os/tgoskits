// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Memory mapping backends.

use ::alloc::vec::Vec;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr};
use ax_memory_set::{
    MapPrecondition, MappingBackend, MappingError, MappingOperation, MappingResult,
};
use axvm_types::{GuestPhysAddr, MappingFlags};

use crate::{AddrSpaceError, NestedPageTableOps, PageSize};

mod alloc;
mod linear;

#[derive(Clone, Copy)]
struct SavedMapping {
    vaddr: GuestPhysAddr,
    paddr: PhysAddr,
    flags: MappingFlags,
    page_size: PageSize,
}

#[doc(hidden)]
pub struct BackendTransaction {
    operation: MappingOperation<GuestPhysAddr, MappingFlags>,
    previous: Vec<SavedMapping>,
}

/// A unified enum type for different memory mapping backends.
///
/// Currently, two backends are implemented:
///
/// - **Linear**: used for linear mappings. The target physical frames are
///   contiguous and their addresses should be known when creating the mapping.
/// - **Allocation**: used in general, or for lazy mappings. The target physical
///   frames are obtained from the global allocator.
pub enum Backend<Npt: NestedPageTableOps> {
    /// Linear mapping backend.
    ///
    /// The offset between the virtual address and the physical address is
    /// constant, which is specified by `pa_to_va_delta`. For example, the
    /// virtual address `vaddr` is mapped to the physical address
    /// `(vaddr as i128 - pa_to_va_delta) as usize`.
    Linear {
        /// `vaddr - paddr`.
        pa_to_va_delta: i128,
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
        /// A phantom data for the nested page table type.
        _phantom: core::marker::PhantomData<Npt>,
    },
}

impl<Npt: NestedPageTableOps> Clone for Backend<Npt> {
    fn clone(&self) -> Self {
        match *self {
            Self::Linear { pa_to_va_delta } => Self::Linear { pa_to_va_delta },
            Self::Alloc { populate, .. } => Self::Alloc {
                populate,
                _phantom: core::marker::PhantomData,
            },
        }
    }
}

impl<Npt: NestedPageTableOps> MappingBackend for Backend<Npt> {
    type Addr = GuestPhysAddr;
    type Flags = MappingFlags;
    type PageTable = Npt;
    type MappingPlan = BackendTransaction;
    type CommitState = BackendTransaction;

    fn prepare(
        &self,
        operation: MappingOperation<GuestPhysAddr, MappingFlags>,
        pt: &mut Npt,
    ) -> MappingResult<Self::MappingPlan> {
        let (start, size) = operation.range();
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let mut previous = Vec::new();
        let mut contains_unmapped = false;
        let mut vaddr = start;
        while vaddr < end {
            match pt.query(vaddr) {
                Ok((paddr, flags, page_size)) => {
                    let mapped_size = usize::from(page_size);
                    let mapping_end = vaddr
                        .checked_add(mapped_size)
                        .ok_or(MappingError::BadState)?;
                    if !vaddr.as_usize().is_multiple_of(mapped_size) || mapping_end > end {
                        return Err(MappingError::BadState);
                    }
                    previous
                        .try_reserve(1)
                        .map_err(|_| MappingError::NoMemory)?;
                    previous.push(SavedMapping {
                        vaddr,
                        paddr,
                        flags,
                        page_size,
                    });
                    vaddr = mapping_end;
                }
                Err(AddrSpaceError::Unmapped { .. }) => {
                    contains_unmapped = true;
                    vaddr = vaddr
                        .checked_add(PAGE_SIZE_4K)
                        .ok_or(MappingError::BadState)?;
                }
                Err(_) => return Err(MappingError::BadState),
            }
        }

        if matches!(
            operation,
            MappingOperation::Map {
                precondition: MapPrecondition::Vacant,
                ..
            }
        ) && !previous.is_empty()
        {
            return Err(MappingError::AlreadyExists);
        }
        if let (Self::Linear { pa_to_va_delta }, MappingOperation::Map { start, size, .. }) =
            (self, operation)
        {
            let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
            if Self::linear_paddr(start, *pa_to_va_delta).is_none()
                || Self::linear_paddr(end, *pa_to_va_delta).is_none()
            {
                return Err(MappingError::InvalidParam);
            }
        }
        if matches!(self, Self::Linear { .. })
            && matches!(operation, MappingOperation::Unmap { .. })
            && contains_unmapped
        {
            return Err(MappingError::BadState);
        }
        Ok(BackendTransaction {
            operation,
            previous,
        })
    }

    fn abort(&self, _plan: Self::MappingPlan, _pt: &mut Npt) {}

    fn commit(&self, plan: Self::MappingPlan, pt: &mut Npt) -> MappingResult<Self::CommitState> {
        if self.apply(plan.operation, pt) {
            Ok(plan)
        } else {
            self.rollback(plan, pt)?;
            Err(MappingError::BadState)
        }
    }

    fn rollback(&self, state: Self::CommitState, pt: &mut Npt) -> MappingResult {
        match state.operation {
            MappingOperation::Map { .. } => {
                let (start, size) = state.operation.range();
                let end = start.checked_add(size).ok_or(MappingError::BadState)?;
                let mut vaddr = start;
                while vaddr < end {
                    match pt.query(vaddr) {
                        Ok((_, _, page_size)) => {
                            let mapped_size = usize::from(page_size);
                            let mapping_end = vaddr
                                .checked_add(mapped_size)
                                .ok_or(MappingError::BadState)?;
                            if !vaddr.as_usize().is_multiple_of(mapped_size) || mapping_end > end {
                                return Err(MappingError::BadState);
                            }
                            let (frame, _, removed_size) =
                                pt.unmap(vaddr).map_err(|_| MappingError::BadState)?;
                            if removed_size != page_size {
                                return Err(MappingError::BadState);
                            }
                            if matches!(self, Self::Alloc { .. }) {
                                if page_size != PageSize::Size4K {
                                    return Err(MappingError::BadState);
                                }
                                pt.dealloc_frame(frame);
                            }
                            vaddr = mapping_end;
                        }
                        Err(AddrSpaceError::Unmapped { .. }) => {
                            vaddr = vaddr
                                .checked_add(PAGE_SIZE_4K)
                                .ok_or(MappingError::BadState)?;
                        }
                        Err(_) => return Err(MappingError::BadState),
                    }
                }
                for saved in state.previous {
                    pt.map(saved.vaddr, saved.paddr, saved.page_size, saved.flags)
                        .map_err(|_| MappingError::BadState)?;
                }
            }
            MappingOperation::Unmap { .. } => {
                for saved in state.previous {
                    match pt.query(saved.vaddr) {
                        Err(AddrSpaceError::Unmapped { .. }) => pt
                            .map(saved.vaddr, saved.paddr, saved.page_size, saved.flags)
                            .map_err(|_| MappingError::BadState)?,
                        Ok((paddr, flags, page_size))
                            if paddr == saved.paddr
                                && flags == saved.flags
                                && page_size == saved.page_size => {}
                        _ => return Err(MappingError::BadState),
                    }
                }
            }
            MappingOperation::Protect { .. } => {
                for saved in state.previous {
                    if !pt.protect_region(saved.vaddr, saved.page_size.into(), saved.flags) {
                        return Err(MappingError::BadState);
                    }
                }
            }
        }
        Ok(())
    }

    fn finalize(&self, state: Self::CommitState, pt: &mut Npt) {
        if matches!(self, Self::Alloc { .. })
            && matches!(state.operation, MappingOperation::Unmap { .. })
        {
            for saved in state.previous {
                debug_assert_eq!(saved.page_size, PageSize::Size4K);
                pt.dealloc_frame(saved.paddr);
            }
        }
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        // backend can be trivially split since it does not have any state.
        Some(self.clone())
    }
}

impl<Npt: NestedPageTableOps> Backend<Npt> {
    fn apply(
        &self,
        operation: MappingOperation<GuestPhysAddr, MappingFlags>,
        page_table: &mut Npt,
    ) -> bool {
        match operation {
            MappingOperation::Map {
                start, size, flags, ..
            } => match *self {
                Self::Linear { pa_to_va_delta } => {
                    self.map_linear(start, size, flags, page_table, pa_to_va_delta)
                }
                Self::Alloc { populate, .. } => {
                    self.map_alloc(start, size, flags, page_table, populate)
                }
            },
            MappingOperation::Unmap { start, size, .. } => match *self {
                Self::Linear { pa_to_va_delta } => {
                    self.unmap_linear(start, size, page_table, pa_to_va_delta)
                }
                Self::Alloc { populate, .. } => self.unmap_alloc(start, size, page_table, populate),
            },
            MappingOperation::Protect {
                start,
                size,
                new_flags,
                ..
            } => page_table.protect_region(start, size, new_flags),
        }
    }

    pub(crate) fn handle_page_fault(
        &self,
        vaddr: GuestPhysAddr,
        orig_flags: MappingFlags,
        page_table: &mut Npt,
    ) -> bool {
        match *self {
            Self::Linear { .. } => false, // Linear mappings should not trigger page faults.
            Self::Alloc { populate, .. } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
        }
    }
}

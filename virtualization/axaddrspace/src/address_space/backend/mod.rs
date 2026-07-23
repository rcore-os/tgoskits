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
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, PhysAddr};
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
}

#[doc(hidden)]
pub struct BackendTransaction {
    operation: MappingOperation<GuestPhysAddr, MappingFlags>,
    previous: Vec<Option<SavedMapping>>,
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
                Err(AddrSpaceError::Unmapped { .. }) => previous.push(None),
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
            && previous.iter().any(Option::is_none)
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
                for (vaddr, previous) in PageIter4K::new(start, end)
                    .ok_or(MappingError::BadState)?
                    .zip(state.previous)
                {
                    match pt.unmap(vaddr) {
                        Ok((frame, _, PageSize::Size4K)) => {
                            if matches!(self, Self::Alloc { .. }) {
                                pt.dealloc_frame(frame);
                            }
                        }
                        Ok(_) | Err(AddrSpaceError::Unmapped { .. }) => {}
                        Err(_) => return Err(MappingError::BadState),
                    }
                    if let Some(saved) = previous {
                        pt.map(saved.vaddr, saved.paddr, PageSize::Size4K, saved.flags)
                            .map_err(|_| MappingError::BadState)?;
                    }
                }
            }
            MappingOperation::Unmap { .. } => {
                for saved in state.previous.into_iter().flatten() {
                    match pt.query(saved.vaddr) {
                        Err(AddrSpaceError::Unmapped { .. }) => pt
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
                    if !pt.protect_region(saved.vaddr, PAGE_SIZE_4K, saved.flags) {
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
            for saved in state.previous.into_iter().flatten() {
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

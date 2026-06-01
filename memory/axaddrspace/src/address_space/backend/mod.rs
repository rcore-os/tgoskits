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

use ax_memory_addr::{MemoryAddr, PhysAddr};
use ax_memory_set::MappingBackend;
use ax_page_table_multiarch::{MappingFlags, PagingHandler};

use crate::{GuestPhysAddr, npt::NestedPageTable as PageTable};

mod alloc;
mod linear;

/// A unified enum type for different memory mapping backends.
///
/// Currently, two backends are implemented:
///
/// - **Linear**: used for linear mappings. The target physical frames are
///   contiguous and their addresses should be known when creating the mapping.
/// - **Allocation**: used in general, or for lazy mappings. The target physical
///   frames are obtained from the global allocator.
pub enum Backend<H: PagingHandler> {
    /// Linear mapping backend.
    ///
    /// The target physical frames are contiguous and begin at `phys_start`.
    /// The current guest-physical start address is tracked by the surrounding
    /// `MemoryArea`, so split/shrink operations only need to update the host
    /// physical start accordingly.
    Linear {
        /// Host physical address that backs the current area's start.
        phys_start: PhysAddr,
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
        /// A phantom data for the paging handler.
        _phantom: core::marker::PhantomData<H>,
    },
}

impl<H: PagingHandler> Clone for Backend<H> {
    fn clone(&self) -> Self {
        match *self {
            Self::Linear { phys_start } => Self::Linear { phys_start },
            Self::Alloc { populate, .. } => Self::Alloc {
                populate,
                _phantom: core::marker::PhantomData,
            },
        }
    }
}

impl<H: PagingHandler> MappingBackend for Backend<H> {
    type Addr = GuestPhysAddr;
    type Flags = MappingFlags;
    type PageTable = PageTable<H>;

    fn map(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable<H>,
    ) -> bool {
        match *self {
            Self::Linear { phys_start } => self.map_linear(start, size, flags, pt, phys_start),
            Self::Alloc { populate, .. } => self.map_alloc(start, size, flags, pt, populate),
        }
    }

    fn unmap(&self, start: GuestPhysAddr, size: usize, pt: &mut PageTable<H>) -> bool {
        match *self {
            Self::Linear { phys_start } => self.unmap_linear(start, size, pt, phys_start),
            Self::Alloc { populate, .. } => self.unmap_alloc(start, size, pt, populate),
        }
    }

    fn protect(
        &self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
        page_table: &mut PageTable<H>,
    ) -> bool {
        page_table.protect_region(start, size, new_flags)
    }

    fn split(&mut self, align_diff: usize) -> Option<Self> {
        match self {
            Self::Linear { phys_start } => Some(Self::Linear {
                phys_start: phys_start.wrapping_add(align_diff),
            }),
            Self::Alloc { .. } => {
                // Alloc backend has no per-range state.
                Some(self.clone())
            }
        }
    }

    fn shrink_left(&mut self, shrink_size: usize) {
        if let Self::Linear { phys_start } = self {
            *phys_start = phys_start.wrapping_add(shrink_size);
        }
    }

    fn shrink_right(&mut self, _shrink_size: usize) {}
}

impl<H: PagingHandler> Backend<H> {
    pub(crate) fn handle_page_fault(
        &self,
        vaddr: GuestPhysAddr,
        orig_flags: MappingFlags,
        page_table: &mut PageTable<H>,
    ) -> bool {
        match *self {
            Self::Linear { .. } => false, // Linear mappings should not trigger page faults.
            Self::Alloc { populate, .. } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
        }
    }
}

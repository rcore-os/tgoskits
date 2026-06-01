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

use ax_memory_addr::{MemoryAddr, PhysAddr};
use ax_page_table_multiarch::{MappingFlags, PagingHandler};

use super::Backend;
use crate::{GuestPhysAddr, npt::NestedPageTable as PageTable};

impl<H: PagingHandler> Backend<H> {
    /// Creates a new linear mapping backend.
    pub const fn new_linear(phys_start: PhysAddr) -> Self {
        Self::Linear { phys_start }
    }

    pub(crate) fn map_linear(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable<H>,
        phys_start: PhysAddr,
    ) -> bool {
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start,
            start + size,
            phys_start,
            phys_start + size,
            flags
        );
        let allow_huge = true;
        pt.map_region(
            start,
            |va| phys_start.add(va.sub_addr(start)),
            size,
            flags,
            allow_huge,
        )
        .is_ok()
    }

    pub(crate) fn unmap_linear(
        &self,
        start: GuestPhysAddr,
        size: usize,
        pt: &mut PageTable<H>,
        _phys_start: PhysAddr,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.unmap_region(start, size).is_ok()
    }
}

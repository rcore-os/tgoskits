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

use memory_addr::PhysAddr;
use page_table_multiarch::{MappingFlags, PagingHandler};

use super::Backend;
use crate::{GuestPhysAddr, npt::NestedPageTable as PageTable};

impl<H: PagingHandler> Backend<H> {
    /// Creates a new linear mapping backend.
    pub const fn new_linear(pa_va_offset: usize) -> Self {
        Self::Linear { pa_va_offset }
    }

    pub(crate) fn map_linear(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable<H>,
        pa_va_offset: usize,
    ) -> bool {
        let pa_start = PhysAddr::from(start.as_usize() - pa_va_offset);
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start,
            start + size,
            pa_start,
            pa_start + size,
            flags
        );
        pt.map_region(
            start,
            |va| PhysAddr::from(va.as_usize() - pa_va_offset),
            size,
            flags,
            true,
            true,
        )
        .is_ok()
    }

    pub(crate) fn unmap_linear(
        &self,
        start: GuestPhysAddr,
        size: usize,
        pt: &mut PageTable<H>,
        _pa_va_offset: usize,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.unmap_region(start, size, true).is_ok()
    }
}

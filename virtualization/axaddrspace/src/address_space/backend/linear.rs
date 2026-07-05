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

use ax_memory_addr::PhysAddr;
use axvm_types::{GuestPhysAddr, MappingFlags};

use super::Backend;
use crate::NestedPageTableOps;

impl<Npt: NestedPageTableOps> Backend<Npt> {
    /// Creates a new linear mapping backend.
    pub const fn new_linear(pa_to_va_delta: i128) -> Self {
        Self::Linear { pa_to_va_delta }
    }

    fn linear_paddr(vaddr: GuestPhysAddr, pa_to_va_delta: i128) -> Option<PhysAddr> {
        let paddr = (vaddr.as_usize() as i128).checked_sub(pa_to_va_delta)?;
        usize::try_from(paddr).ok().map(PhysAddr::from)
    }

    pub(crate) fn map_linear(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut Npt,
        pa_to_va_delta: i128,
    ) -> bool {
        let Some(pa_start) = Self::linear_paddr(start, pa_to_va_delta) else {
            return false;
        };
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start,
            start + size,
            pa_start,
            pa_start + size,
            flags
        );
        let allow_huge = true;
        pt.map_region(
            start,
            |va| {
                Self::linear_paddr(va, pa_to_va_delta)
                    .expect("linear mapping physical address underflow")
            },
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
        pt: &mut Npt,
        _pa_to_va_delta: i128,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.unmap_region(start, size).is_ok()
    }
}

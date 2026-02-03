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

use crate::{HostPhysAddr, HostVirtAddr};

/// Hardware abstraction layer for memory management.
pub trait AxMmHal {
    /// Allocates a frame and returns its host physical address. The
    ///
    /// # Returns
    ///
    /// * `Option<HostPhysAddr>` - Some containing the physical address of the allocated frame, or None if allocation fails.
    fn alloc_frame() -> Option<HostPhysAddr>;

    /// Deallocates a frame given its physical address.
    ///
    /// # Parameters
    ///
    /// * `paddr` - The physical address of the frame to deallocate.
    fn dealloc_frame(paddr: HostPhysAddr);

    /// Converts a host physical address to a host virtual address.
    ///
    /// # Parameters
    ///
    /// * `paddr` - The physical address to convert.
    ///
    /// # Returns
    ///
    /// * `HostVirtAddr` - The corresponding virtual address.
    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr;

    /// Converts a host virtual address to a host physical address.
    ///
    /// # Parameters
    ///
    /// * `vaddr` - The virtual address to convert.
    ///
    /// # Returns
    ///
    /// * `HostPhysAddr` - The corresponding physical address.
    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr;
}

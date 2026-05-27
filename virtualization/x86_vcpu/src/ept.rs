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

#[derive(Debug)]
/// The information of guest page walk.
pub struct GuestPageWalkInfo {
    /// The guest page table physical address.
    pub top_entry: usize, // Top level paging structure entry
    /// Guest page table level.
    pub level: usize,
    /// Guest page table width
    pub width: u32,
    /// Guest page table user mode
    pub is_user_mode_access: bool,
    /// Guest page table write access
    pub is_write_access: bool,
    /// Guest page table instruction fetch
    pub is_inst_fetch: bool,
    /// CR4.PSE for 32bit paging, true for PAE/4-level paging
    pub pse: bool,
    /// CR0.WP
    pub wp: bool, // CR0.WP
    /// MSR_IA32_EFER_NXE_BIT
    pub nxe: bool,

    /// Guest page table Supervisor mode access prevention
    pub is_smap_on: bool,
    /// Guest page table Supervisor mode execution protection
    pub is_smep_on: bool,
}

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

//! Platform and firmware discovery APIs.

use ax_errno::AxResult;

use crate::memory::PhysAddr;

/// Platform-specific APIs used by AxVisor during boot and host resource handoff.
#[crate::api_def]
pub trait PlatformIf {
    /// Returns the physical address of the host-provided FDT blob, if any.
    fn get_host_fdt_ptr() -> Option<PhysAddr>;

    /// Shut down the host filesystem stack before handing storage resources to
    /// guests.
    fn shutdown_host_filesystems() -> AxResult;
}

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

//! Migration helpers that depend on concrete device-crate types.
//!
//! These will move out of `axdevice` as the migration progresses.

// ---------------------------------------------------------------------------
// vtimer helper (will move to axvm — it depends on arm_vgic concrete types)
// ---------------------------------------------------------------------------

/// creates the standard set of vTimer system-register devices, each wrapped
/// in a [`SysRegDeviceAdapter`](axdevice_base::SysRegDeviceAdapter) so they
/// implement [`Device`].
#[cfg(target_arch = "aarch64")]
pub fn create_vtimer_devices() -> alloc::vec::Vec<alloc::boxed::Box<dyn axdevice_base::Device>> {
    use arm_vgic::vtimer::{SysCntpCtlEl0, SysCntpTvalEl0, SysCntpctEl0};
    use axdevice_base::SysRegDeviceAdapter;

    let mut devs: alloc::vec::Vec<alloc::boxed::Box<dyn axdevice_base::Device>> =
        alloc::vec::Vec::new();
    devs.push(alloc::boxed::Box::new(SysRegDeviceAdapter::new(
        SysCntpCtlEl0::new(),
    )));
    devs.push(alloc::boxed::Box::new(SysRegDeviceAdapter::new(
        SysCntpctEl0::new(),
    )));
    devs.push(alloc::boxed::Box::new(SysRegDeviceAdapter::new(
        SysCntpTvalEl0::new(),
    )));
    devs
}

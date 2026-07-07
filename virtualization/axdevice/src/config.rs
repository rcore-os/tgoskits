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

use alloc::{sync::Arc, vec::Vec};

use axvmconfig::EmulatedDeviceConfig;
use vm_interrupt::VmInterruptRouter;

/// The vector of DeviceConfig
pub struct AxVmDeviceConfig {
    /// Interrupt routing endpoint for devices owned by this VM.
    pub interrupt_router: Option<Arc<dyn VmInterruptRouter>>,
    /// Guest topology map: `(vCPU ID, guest CPU/hart ID)`.
    pub guest_cpu_topology: Vec<(usize, usize)>,
    /// The vector of EmulatedDeviceConfig
    pub emu_configs: Vec<EmulatedDeviceConfig>,
}

/// The implemention for AxVmDeviceConfig
impl AxVmDeviceConfig {
    /// The new function for AxVmDeviceConfig
    pub fn new(emu_configs: Vec<EmulatedDeviceConfig>) -> Self {
        Self {
            interrupt_router: None,
            guest_cpu_topology: Vec::new(),
            emu_configs,
        }
    }
}

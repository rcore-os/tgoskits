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

//! Host platform interrupt hooks that are not part of a VM topology.

/// Host platform hook for registering the RISC-V physical IRQ injector.
#[ax_crate_interface::def_interface]
pub trait RiscvPlatformIrqInjectorIf {
    /// Registers a callback that forwards a physical IRQ line into the current guest.
    fn register_virtual_irq_injector(injector: fn(usize) -> bool);

    /// Routes physical PLIC IRQs that may be forwarded to a guest toward the vCPU CPU.
    fn set_virtual_irq_targets(cpu_id: usize, irq_sources: &[u32]);
}

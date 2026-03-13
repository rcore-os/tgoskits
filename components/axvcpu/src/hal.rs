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

/// Hardware abstraction layer interfaces for VCpu operations.
///
/// This trait defines the interfaces that the underlying software (kernel or hypervisor)
/// must implement to support VCpu operations such as interrupt handling and memory management.
pub trait AxVCpuHal {
    /// Memory management interfaces required by the VCpu subsystem.
    /// Must implement the AxMmHal trait from the axaddrspace crate.
    type MmHal: axaddrspace::AxMmHal;

    /// Fetches the current interrupt (IRQ) number from the hardware.
    fn irq_fetch() -> usize {
        0
    }

    /// Dispatches an interrupt request (IRQ) to the underlying host OS.
    ///
    /// This function should handle the actual interrupt processing and delegation
    /// to the appropriate interrupt handler in the host system.
    ///
    /// # Implementation Required
    ///
    /// The default implementation panics as this function **must** be implemented
    /// by the underlying hypervisor or kernel to provide proper interrupt handling.
    ///
    /// # Safety
    ///
    /// Implementations should ensure proper interrupt handling and avoid
    /// reentrancy issues during interrupt processing.
    fn irq_hanlder() {
        unimplemented!("irq_handler is not implemented");
    }
}

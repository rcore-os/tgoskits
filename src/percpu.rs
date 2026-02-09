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

use core::mem::MaybeUninit;

use axerrno::{AxResult, ax_err};

/// Trait representing the per-CPU architecture-specific virtualization state in a virtual machine.
///
/// This trait defines the required methods to manage and interact with the virtualization
/// state on a per-CPU basis. Implementers of this trait should handle the specifics of how
/// hardware virtualization is enabled, disabled, and checked for each CPU in the system.
pub trait AxArchPerCpu: Sized {
    /// Create a new per-CPU state.
    fn new(cpu_id: usize) -> AxResult<Self>;
    /// Whether hardware virtualization is enabled on the current CPU.
    fn is_enabled(&self) -> bool;
    /// Enable hardware virtualization on the current CPU.
    fn hardware_enable(&mut self) -> AxResult;
    /// Disable hardware virtualization on the current CPU.
    fn hardware_disable(&mut self) -> AxResult;
    /// Return max guest page table levels used by the architecture.
    fn max_guest_page_table_levels(&self) -> usize {
        4
    }
}

/// Host per-CPU states to run the guest.
///
/// Recommended usage:
/// - Define a per-CPU state in hypervisor:
///
///   ```ignore
///   #[percpu::def_percpu]
///   pub static AXVM_PER_CPU: AxPerCpu<MyArchPerCpuImpl> = AxPerCpu::new_uninit();
///   ```
///
/// - Then initialize and enable virtualization on each CPU in the hypervisor initialization code:
///
///   ```ignore
///   let percpu = unsafe {
///       AXVM_PER_CPU.current_ref_mut_raw()
///   };
///   percpu.init(0).expect("Failed to initialize percpu state");
///   percpu.hardware_enable().expect("Failed to enable virtualization");
///   ```
pub struct AxPerCpu<A: AxArchPerCpu> {
    /// The id of the CPU. It's also used to check whether the per-CPU state is initialized.
    cpu_id: Option<usize>,
    /// The architecture-specific per-CPU state.
    arch: MaybeUninit<A>,
}

impl<A: AxArchPerCpu> AxPerCpu<A> {
    /// Create a new, uninitialized per-CPU state.
    pub const fn new_uninit() -> Self {
        Self {
            cpu_id: None,
            arch: MaybeUninit::uninit(),
        }
    }

    /// Initialize the per-CPU state.
    pub fn init(&mut self, cpu_id: usize) -> AxResult {
        if self.cpu_id.is_some() {
            ax_err!(BadState, "per-CPU state is already initialized")
        } else {
            self.cpu_id = Some(cpu_id);
            self.arch.write(A::new(cpu_id)?);
            Ok(())
        }
    }

    /// Return the architecture-specific per-CPU state. Panics if the per-CPU state is not initialized.
    pub fn arch_checked(&self) -> &A {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        // SAFETY: `cpu_id` is `Some` here, so `arch` must be initialized.
        unsafe { self.arch.assume_init_ref() }
    }

    /// Return the mutable architecture-specific per-CPU state. Panics if the per-CPU state is not initialized.
    pub fn arch_checked_mut(&mut self) -> &mut A {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        // SAFETY: `cpu_id` is `Some` here, so `arch` must be initialized.
        unsafe { self.arch.assume_init_mut() }
    }

    /// Whether the current CPU has hardware virtualization enabled.
    pub fn is_enabled(&self) -> bool {
        self.arch_checked().is_enabled()
    }

    /// Enable hardware virtualization on the current CPU.
    pub fn hardware_enable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_enable()
    }

    /// Disable hardware virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_disable()
    }
}

impl<A: AxArchPerCpu> Drop for AxPerCpu<A> {
    fn drop(&mut self) {
        if self.is_enabled() {
            self.hardware_disable().unwrap();
        }
    }
}

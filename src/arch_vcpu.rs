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

use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axerrno::AxResult;
use axvisor_api::vmm::{VCpuId, VMId};

use crate::exit::AxVCpuExitReason;

/// Architecture-specific virtual CPU trait definition.
///
/// This trait provides an abstraction layer for implementing virtual CPUs across
/// different hardware architectures (x86_64, ARM64, RISC-V, etc.). Each architecture
/// must implement this trait to provide the necessary low-level virtualization primitives.
pub trait AxArchVCpu: Sized {
    /// Architecture-specific configuration for VCpu creation.
    ///
    /// This associated type allows each architecture to define its own
    /// configuration parameters needed during VCpu initialization.
    type CreateConfig;

    /// Architecture-specific configuration for VCpu setup.
    ///
    /// This associated type allows each architecture to specify additional
    /// configuration parameters needed after basic VCpu creation but before execution.
    type SetupConfig;

    /// Creates a new architecture-specific VCpu instance.
    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self>;

    /// Sets the guest entry point where VCpu execution will begin.
    ///
    /// It's guaranteed that this function is called only once, before [`AxArchVCpu::setup`] being called.
    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult;

    /// Sets the Extended Page Table (EPT) root for memory translation.
    ///
    /// The EPT root defines the top-level page table used for guest-to-host
    /// physical address translation in hardware virtualization.
    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult;

    /// Completes VCpu initialization and prepares it for execution.
    ///
    /// This method performs any final architecture-specific setup needed
    /// before the VCpu can be bound and executed.
    fn setup(&mut self, config: Self::SetupConfig) -> AxResult;

    /// Executes the VCpu until a VM exit occurs.
    ///
    /// This is the core execution method that transfers control to the guest VCpu
    /// and runs until the guest triggers a VM exit condition that requires
    /// hypervisor intervention.
    fn run(&mut self) -> AxResult<AxVCpuExitReason>;

    /// Binds the VCpu to the current physical CPU for execution.
    ///
    /// This method performs any necessary architecture-specific initialization
    /// to prepare the VCpu for execution on the current physical CPU.
    fn bind(&mut self) -> AxResult;

    /// Unbinds the VCpu from the current physical CPU.
    ///
    /// This method performs cleanup and state preservation when moving
    /// the VCpu away from the current physical CPU.
    fn unbind(&mut self) -> AxResult;

    /// Sets the value of a general-purpose register.
    fn set_gpr(&mut self, reg: usize, val: usize);

    /// Inject an interrupt to the VCpu.
    ///
    /// It's guaranteed (for implementors, and required for callers) that this function is called
    /// on the physical CPU where the VCpu is running or queueing.
    ///
    /// It's not guaranteed that the VCpu is running or bound to the current physical CPU when this
    /// function is called. It means sometimes an irq queue is necessary to buffer the interrupts
    /// until the VCpu is running.
    fn inject_interrupt(&mut self, vector: usize) -> AxResult;

    /// Sets the return value that will be delivered to the guest.
    fn set_return_value(&mut self, val: usize);
}

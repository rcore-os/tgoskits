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

//! Virtual machine monitor APIs provided by AxVisor core.
//!
//! This module provides APIs for querying VM/vCPU topology, injecting virtual
//! interrupts, and scheduling VMM timers.
//!
//! # Overview
//!
//! The VMM (Virtual Machine Monitor) APIs enable lower-level virtualization
//! components to:
//! - Get information about VMs and their vCPUs
//! - Inject interrupts into virtual CPUs
//! - Register and cancel virtual timer callbacks
//!
//! Current VM/vCPU context is maintained inside `axvisor-core` and is not part
//! of the host tasking contract.
//!
//! # Types
//!
//! - [`VMId`] - Virtual machine identifier.
//! - [`VCpuId`] - Virtual CPU identifier.
//! - [`InterruptVector`] - Interrupt vector number.
//! - [`CancelToken`] - Token used to cancel a registered VMM timer.
//!
//! # Helper Functions
//!
//! In addition to the core API trait, this module provides helper functions:
//! - [`vm_vcpu_num`] - Get the vCPU count of a VM.
//! - [`vm_active_vcpus`] - Get the active vCPU mask of a VM.
//!
//! # Implementation
//!
//! These APIs are implemented by `axvisor-core`, not by host OS adapters.
//!
//! Lower-level virtualization components call this module to query VM topology
//! and request interrupt injection without depending directly on
//! `axvisor-core`.

extern crate alloc;

use alloc::boxed::Box;

use crate::time::TimeValue;

/// Virtual machine identifier type.
///
/// Each virtual machine is assigned a unique identifier that can be used to
/// reference it in API calls.
pub type VMId = usize;

/// Virtual CPU identifier type.
///
/// Each vCPU within a VM is assigned a unique identifier, usually 0-indexed
/// within the VM.
pub type VCpuId = usize;

/// Interrupt vector type used by VMM interrupt injection APIs.
pub type InterruptVector = u8;

/// The maximum number of virtual CPUs supported in a virtual machine.
pub const MAX_VCPU_NUM: usize = 64;

/// A set of virtual CPUs.
pub type VCpuSet = ax_cpumask::CpuMask<MAX_VCPU_NUM>;

/// Cancel token type for VMM timer cancellation.
///
/// This token is returned when registering a VMM timer and can be used to
/// cancel the timer before it fires.
pub type CancelToken = usize;

/// The API trait for virtual machine management functionalities.
///
/// This trait defines the core VM management interface required by the
/// hypervisor components. This interface is implemented by Axvisor core.
#[crate::api_def]
pub trait VmmIf {
    /// Get the number of virtual CPUs in a virtual machine.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The identifier of the virtual machine to query.
    ///
    /// # Returns
    ///
    /// - `Some(count)` - The number of vCPUs in the specified VM.
    /// - `None` - If the VM ID is invalid.
    fn vcpu_num(vm_id: VMId) -> Option<usize>;

    /// Get the bitmask of active virtual CPUs in a virtual machine.
    ///
    /// Each bit in the returned value represents a vCPU, where bit N is set
    /// if vCPU N is active (online and running).
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The identifier of the virtual machine to query.
    ///
    /// # Returns
    ///
    /// - `Some(mask)` - The active vCPU bitmask for the specified VM.
    /// - `None` - If the VM ID is invalid.
    fn active_vcpus(vm_id: VMId) -> Option<usize>;

    /// Inject an interrupt into a specific virtual CPU.
    ///
    /// This function queues an interrupt to be delivered to the specified
    /// vCPU when it is next scheduled.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The identifier of the target virtual machine.
    /// * `vcpu_id` - The identifier of the target virtual CPU.
    /// * `vector` - The interrupt vector to inject.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axvisor_api::vmm::inject_interrupt;
    ///
    /// // Inject timer interrupt (vector 0x20) to vCPU 0 of the current VM
    /// inject_interrupt(vm_id, 0, 0x20);
    /// ```
    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector);

    /// Inject an interrupt to a set of virtual CPUs.
    fn inject_interrupt_to_cpus(vm_id: VMId, vcpu_set: VCpuSet, vector: InterruptVector);

    /// Register a VMM timer that will fire at the specified deadline.
    ///
    /// When the deadline is reached, the callback function will be called
    /// with the actual time at which it was invoked.
    ///
    /// # Arguments
    ///
    /// * `deadline` - The monotonic time at which the timer should fire.
    /// * `callback` - The function to call when the timer fires. It receives
    ///   the actual time as an argument.
    ///
    /// # Returns
    ///
    /// A [`CancelToken`] that can be used to cancel the timer with
    /// [`cancel_timer`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axvisor_api::{
    ///     time::current_time,
    ///     vmm::register_timer,
    /// };
    /// use core::time::Duration;
    ///
    /// let deadline = current_time() + Duration::from_millis(100);
    /// let token = register_timer(deadline, Box::new(|actual_time| {
    ///     println!("Timer fired at {:?}", actual_time);
    /// }));
    /// ```
    fn register_timer(
        deadline: TimeValue,
        callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> CancelToken;

    /// Cancel a previously registered VMM timer.
    ///
    /// If the timer has already fired, this function has no effect.
    ///
    /// # Arguments
    ///
    /// * `token` - The cancel token returned by [`register_timer`].
    fn cancel_timer(token: CancelToken);
}

/// Get the number of virtual CPUs in the given virtual machine.
///
/// # Returns
///
/// The number of vCPUs in the current VM.
///
/// # Panics
///
/// Panics if called outside of a valid VM context.
pub fn vm_vcpu_num(vm_id: VMId) -> usize {
    vcpu_num(vm_id).unwrap()
}

/// Get the bitmask of active virtual CPUs in the given virtual machine.
///
/// # Returns
///
/// The active vCPU bitmask for the current VM.
///
/// # Panics
///
/// Panics if called outside of a valid VM context.
pub fn vm_active_vcpus(vm_id: VMId) -> usize {
    active_vcpus(vm_id).unwrap()
}

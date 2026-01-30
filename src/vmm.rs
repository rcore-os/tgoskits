//! Virtual machine management APIs for the AxVisor hypervisor.
//!
//! This module provides APIs for managing virtual machines (VMs) and virtual
//! CPUs (vCPUs), including querying VM/vCPU information and interrupt
//! injection.
//!
//! # Overview
//!
//! The VMM (Virtual Machine Monitor) APIs enable:
//! - Querying the current VM and vCPU context
//! - Getting information about VMs and their vCPUs
//! - Injecting interrupts into virtual CPUs
//! - Timer expiration notifications
//!
//! # Types
//!
//! - [`VMId`] - Virtual machine identifier.
//! - [`VCpuId`] - Virtual CPU identifier.
//! - [`InterruptVector`] - Interrupt vector number.
//!
//! # Helper Functions
//!
//! In addition to the core API trait, this module provides helper functions:
//! - [`current_vm_vcpu_num`] - Get the vCPU count of the current VM.
//! - [`current_vm_active_vcpus`] - Get the active vCPU mask of the current VM.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct VmmIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::vmm::VmmIf for VmmIfImpl {
//!     fn current_vm_id() -> VMId {
//!         // Return the current VM's ID
//!     }
//!     // ... implement other functions
//! }
//! ```

/// Virtual machine identifier type.
///
/// Each virtual machine is assigned a unique identifier that can be used
/// to reference it in API calls.
pub type VMId = usize;

/// Virtual CPU identifier type.
///
/// Each vCPU within a VM is assigned a unique identifier (0-indexed).
pub type VCpuId = usize;

/// Interrupt vector type.
///
/// Represents the interrupt vector number to be injected into a guest.
pub type InterruptVector = u8;

/// The API trait for virtual machine management functionalities.
///
/// This trait defines the core VM management interface required by the
/// hypervisor components. Implementations should be provided by the VMM
/// layer.
#[crate::api_def]
pub trait VmmIf {
    /// Get the identifier of the current virtual machine.
    ///
    /// This function returns the VM ID of the VM that the calling context
    /// belongs to.
    ///
    /// # Returns
    ///
    /// The current VM's identifier.
    fn current_vm_id() -> VMId;

    /// Get the identifier of the current virtual CPU.
    ///
    /// This function returns the vCPU ID within the current VM context.
    ///
    /// # Returns
    ///
    /// The current vCPU's identifier (0-indexed within the VM).
    fn current_vcpu_id() -> VCpuId;

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
    /// use axvisor_api::vmm::{inject_interrupt, current_vm_id};
    ///
    /// // Inject timer interrupt (vector 0x20) to vCPU 0 of the current VM
    /// inject_interrupt(current_vm_id(), 0, 0x20);
    /// ```
    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector);

    /// Notify that a virtual CPU's timer has expired.
    ///
    /// This function is called when a vCPU's virtual timer expires and needs
    /// to be handled.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The identifier of the virtual machine.
    /// * `vcpu_id` - The identifier of the virtual CPU whose timer expired.
    ///
    /// # Note
    ///
    /// This API may be revised in future versions as the timer virtualization
    /// design evolves.
    fn notify_vcpu_timer_expired(vm_id: VMId, vcpu_id: VCpuId);
}

/// Get the number of virtual CPUs in the current virtual machine.
///
/// This is a convenience function that combines [`current_vm_id`] and
/// [`vcpu_num`].
///
/// # Returns
///
/// The number of vCPUs in the current VM.
///
/// # Panics
///
/// Panics if called outside of a valid VM context (when [`current_vm_id`]
/// returns an invalid ID).
pub fn current_vm_vcpu_num() -> usize {
    vcpu_num(current_vm_id()).unwrap()
}

/// Get the bitmask of active virtual CPUs in the current virtual machine.
///
/// This is a convenience function that combines [`current_vm_id`] and
/// [`active_vcpus`].
///
/// # Returns
///
/// The active vCPU bitmask for the current VM.
///
/// # Panics
///
/// Panics if called outside of a valid VM context (when [`current_vm_id`]
/// returns an invalid ID).
pub fn current_vm_active_vcpus() -> usize {
    active_vcpus(current_vm_id()).unwrap()
}

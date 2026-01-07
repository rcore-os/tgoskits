//! Virtual machine management APIs.

/// Virtual machine ID.
pub type VMId = usize;
/// Virtual CPU ID.
pub type VCpuId = usize;
/// Interrupt vector.
pub type InterruptVector = u8;

/// The API trait for virtual machine management functionalities.
#[crate::api_def]
pub trait VmmIf {
    /// Get the ID of the current virtual machine.
    fn current_vm_id() -> VMId;
    /// Get the ID of the current virtual CPU.
    fn current_vcpu_id() -> VCpuId;
    /// Get the number of virtual CPUs in a virtual machine.
    fn vcpu_num(vm_id: VMId) -> Option<usize>;
    /// Get the mask of active virtual CPUs in a virtual machine.
    fn active_vcpus(vm_id: VMId) -> Option<usize>;
    /// Inject an interrupt to a virtual CPU.
    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector);
    /// Notify that a virtual CPU timer has expired.
    ///
    /// TODO: determine whether we can skip this function.
    fn notify_vcpu_timer_expired(vm_id: VMId, vcpu_id: VCpuId);
}

/// Get the number of virtual CPUs in the current virtual machine.
pub fn current_vm_vcpu_num() -> usize {
    vcpu_num(current_vm_id()).unwrap()
}

/// Get the mask of active virtual CPUs in the current virtual machine.
pub fn current_vm_active_vcpus() -> usize {
    active_vcpus(current_vm_id()).unwrap()
}

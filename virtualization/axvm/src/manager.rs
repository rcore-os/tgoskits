//! AxVM runtime services backed by the default ArceOS host.

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::VMId;

use crate::{
    AxVmError, AxVmResult, ax_err,
    current_vcpu::CurrentVcpuInterruptError,
    host::{HostPlatform, default_host},
    vcpu::{current_vcpu_identity, publish_current_vcpu_interrupt},
    vm::AxVMRef,
};

/// AxVM runtime services.
///
/// The runtime owns host initialization and VM execution primitives. VM-set
/// orchestration belongs to the top-level hypervisor program.
pub struct AxvmRuntime {
    _private: (),
}

static VM_REGISTRY: Mutex<BTreeMap<VMId, AxVMRef>> = Mutex::new(BTreeMap::new());

/// Register an externally initialized VM and return whether it was inserted.
pub(crate) fn push_existing_vm(vm: AxVMRef) -> bool {
    let vm_id = vm.id();
    let mut registry = VM_REGISTRY.lock();
    if registry.contains_key(&vm_id) {
        warn!("VM[{vm_id}] already exists, push VM failed");
        return false;
    }
    registry.insert(vm_id, vm);
    true
}

/// Remove a VM from the process-wide AxVM runtime registry.
pub(crate) fn remove_existing_vm(vm_id: VMId) -> Option<AxVMRef> {
    crate::runtime::vcpus::cleanup_vm_vcpus(vm_id);
    VM_REGISTRY.lock().remove(&vm_id)
}

/// Return a VM from the process-wide AxVM runtime registry.
pub fn get_vm_by_id(vm_id: VMId) -> Option<AxVMRef> {
    VM_REGISTRY.lock().get(&vm_id).cloned()
}

/// Return all VMs known to the process-wide AxVM runtime registry.
pub fn get_vm_list() -> Vec<AxVMRef> {
    VM_REGISTRY.lock().values().cloned().collect()
}

/// Inject a virtual interrupt into a VM's vCPU.
pub fn inject_vm_vcpu_interrupt(vm_id: VMId, vcpu_id: usize, vector: usize) -> AxVmResult {
    if current_vcpu_identity()
        .is_some_and(|identity| identity.vm_id() == vm_id && identity.vcpu_id() == vcpu_id)
    {
        return inject_current_vcpu_interrupt(vector);
    }

    crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector)
}

/// Return the current VM ID from the vCPU currently executing on this CPU.
pub fn current_vm_id() -> Option<VMId> {
    current_vcpu_identity().map(|identity| identity.vm_id())
}

/// Return the current vCPU ID from the vCPU currently executing on this CPU.
pub fn current_vcpu_id() -> Option<usize> {
    current_vcpu_identity().map(|identity| identity.vcpu_id())
}

/// Inject a virtual interrupt into the vCPU currently executing on this CPU.
pub fn inject_current_vcpu_interrupt(vector: usize) -> AxVmResult {
    match publish_current_vcpu_interrupt(vector).map_err(|error| match error {
        CurrentVcpuInterruptError::VectorOutOfRange { vector } => {
            AxVmError::CurrentVcpuInterruptOutOfRange { vector }
        }
    })? {
        true => Ok(()),
        false => Err(AxVmError::CurrentVcpuUnavailable),
    }
}

impl AxvmRuntime {
    /// Create a new AxVM runtime backed by the default ArceOS host adapter.
    pub fn new() -> AxVmResult<Self> {
        let host = default_host();
        if !host.has_hardware_support() {
            return ax_err!(Unsupported, "hardware virtualization is not supported");
        }
        host.enable_virtualization_on_all_cpus()?;
        Ok(Self { _private: () })
    }

    /// Initialize runtime state for already registered VMs.
    pub fn init_vms(&self) {
        crate::runtime::init();
    }

    /// Start all initialized default VMs and wait for them to stop.
    pub fn start_default_vms(&self) {
        crate::runtime::start();
    }

    /// Run an operation with a VM selected from the runtime registry.
    pub fn with_vm<T>(vm_id: VMId, f: impl FnOnce(AxVMRef) -> T) -> Option<T> {
        crate::get_vm_by_id(vm_id).map(f)
    }

    /// Start a VM selected from the runtime registry.
    pub fn start_vm(vm_id: VMId) -> AxVmResult {
        crate::runtime::start_vm(vm_id)
    }

    /// Stop a VM selected from the runtime registry.
    pub fn stop_vm(vm_id: VMId) -> AxVmResult {
        crate::runtime::stop_vm(vm_id)
    }

    /// Resume a VM selected from the runtime registry.
    pub fn resume_vm(vm_id: VMId) -> AxVmResult {
        crate::runtime::resume_vm(vm_id)
    }

    /// Reset a VM selected from the runtime registry.
    pub fn reset_vm(vm_id: VMId) -> AxVmResult {
        crate::runtime::reset_vm(vm_id)
    }

    /// Remove a VM selected from the runtime registry.
    pub fn remove_vm(vm_id: VMId) -> Option<AxVMRef> {
        crate::runtime::remove_vm(vm_id)
    }
}

/// Register a prepared VM in the AxVM runtime.
pub fn register_vm(vm: AxVMRef) -> bool {
    crate::runtime::register_vm(vm)
}

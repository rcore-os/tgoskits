//! AxVM runtime services backed by the default ArceOS host.

use std::{collections::BTreeMap, vec::Vec};

use ax_std::os::arceos::sync::IrqSafeMutex as Mutex;
use axvm_types::VMId;

use crate::{
    AxVmError, AxVmResult,
    arch::ArchVCpu,
    ax_err,
    host::{HostPlatform, default_host},
    vcpu::with_current_vcpu,
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

/// Run an operation with a VM selected from the process-wide runtime registry.
pub(crate) fn with_vm<F, R>(vm_id: VMId, f: F) -> Option<R>
where
    F: FnOnce(&AxVMRef) -> R,
{
    let vm = VM_REGISTRY.lock().get(&vm_id).cloned();
    vm.map(|vm| f(&vm))
}

/// Return the active-vCPU mask for a VM.
pub(crate) fn active_vcpu_mask(vm_id: VMId) -> Option<usize> {
    with_vm(vm_id, |vm| {
        let vcpu_num = vm.vcpu_num();
        if vcpu_num >= usize::BITS as usize {
            usize::MAX
        } else {
            (1usize << vcpu_num) - 1
        }
    })
}

/// Inject a virtual interrupt into a VM's vCPU.
pub(crate) fn inject_interrupt(vm_id: VMId, vcpu_id: usize, vector: usize) -> AxVmResult {
    crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector)
}

/// Wake and kick a target vCPU whose architecture backend already published
/// pending interrupt state.
pub fn notify_vm_vcpu(vm_id: VMId, vcpu_id: usize) -> AxVmResult {
    crate::runtime::vcpus::notify_vcpu(vm_id, vcpu_id)
}

/// Return the current VM ID from the vCPU currently executing on this CPU.
pub fn current_vm_id() -> Option<VMId> {
    with_current_vcpu::<ArchVCpu, _>(|vcpu| vcpu.map(|vcpu| vcpu.vm_id()))
}

/// Return the current vCPU ID from the vCPU currently executing on this CPU.
pub fn current_vcpu_id() -> Option<usize> {
    with_current_vcpu::<ArchVCpu, _>(|vcpu| vcpu.map(|vcpu| vcpu.id()))
}

/// Publish an interrupt for the vCPU currently executing on this CPU.
///
/// Unlike [`inject_current_vcpu_interrupt`], this path does not access
/// CPU-local virtual interrupt-controller state from the host IRQ handler.
/// It publishes the interrupt to the target runtime first, then wakes and
/// kicks the target vCPU. The vCPU owner drains the interrupt immediately
/// before the next guest entry.
pub fn dispatch_current_vcpu_interrupt(vector: usize) -> AxVmResult {
    let (vm_id, vcpu_id) =
        with_current_vcpu::<ArchVCpu, _>(|vcpu| vcpu.map(|vcpu| (vcpu.vm_id(), vcpu.id())))
            .ok_or_else(|| {
                AxVmError::resource_unavailable("current vCPU", "current vCPU is not set")
            })?;
    crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector)
}

/// Inject a virtual interrupt into the vCPU currently executing on this CPU.
pub fn inject_current_vcpu_interrupt(vector: usize) -> AxVmResult {
    with_current_vcpu::<ArchVCpu, _>(|vcpu| {
        let vcpu = vcpu.ok_or_else(|| {
            AxVmError::resource_unavailable("current vCPU", "current vCPU is not set")
        })?;
        vcpu.inject_interrupt(vector)
    })
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

    /// Start all initialized default VMs without waiting for completion.
    pub fn launch_default_vms(&self) -> Vec<VMId> {
        crate::runtime::launch_all()
    }

    /// Wait until all running VMs have stopped.
    pub fn wait_for_all_vms() {
        crate::runtime::wait_for_all();
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

    /// Wake the primary vCPU of a VM.
    pub fn notify_vm(vm_id: VMId) -> AxVmResult {
        crate::runtime::notify_vm(vm_id)
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

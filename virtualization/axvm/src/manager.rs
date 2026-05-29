//! AxVM runtime services backed by the default ArceOS host.

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};

use ax_errno::{AxResult, ax_err, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
use axvcpu::get_current_vcpu;
use axvm_types::VMId;

use crate::{
    host::{HostPlatform, arceos::ArceOsHost},
    vcpu::AxArchVCpuImpl,
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
    let vm = VM_REGISTRY.lock().remove(&vm_id)?;
    crate::runtime::vcpus::cleanup_vm_vcpus(vm_id);
    Some(vm)
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
#[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
pub(crate) fn with_vm<F, R>(vm_id: VMId, f: F) -> Option<R>
where
    F: FnOnce(&AxVMRef) -> R,
{
    let vm = VM_REGISTRY.lock().get(&vm_id).cloned();
    vm.map(|vm| f(&vm))
}

/// Return the active-vCPU mask for a VM.
#[cfg(target_arch = "x86_64")]
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
#[cfg(target_arch = "x86_64")]
pub(crate) fn inject_interrupt(vm_id: VMId, vcpu_id: usize, vector: usize) -> AxResult {
    with_vm(vm_id, |vm| {
        let vcpu = vm
            .vcpu(vcpu_id)
            .ok_or_else(|| ax_err_type!(NotFound, "vCPU not found"))?;
        vcpu.inject_interrupt(vector)
    })
    .unwrap_or_else(|| ax_err!(NotFound, "VM not found"))
}

/// Return the current VM ID from the vCPU currently executing on this CPU.
pub fn current_vm_id() -> Option<VMId> {
    get_current_vcpu::<AxArchVCpuImpl>().map(|vcpu| vcpu.vm_id())
}

/// Return the current vCPU ID from the vCPU currently executing on this CPU.
pub fn current_vcpu_id() -> Option<usize> {
    get_current_vcpu::<AxArchVCpuImpl>().map(|vcpu| vcpu.id())
}

/// Inject a virtual interrupt into the vCPU currently executing on this CPU.
pub fn inject_current_vcpu_interrupt(vector: usize) -> AxResult {
    let vcpu = get_current_vcpu::<AxArchVCpuImpl>()
        .ok_or_else(|| ax_err_type!(BadState, "current vCPU is not set"))?;
    vcpu.inject_interrupt(vector)
}

impl AxvmRuntime {
    /// Create a new AxVM runtime backed by the default ArceOS host adapter.
    pub fn new() -> AxResult<Self> {
        let host = ArceOsHost::new();
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
        crate::runtime::with_vm(vm_id, f)
    }

    /// Start a VM selected from the runtime registry.
    pub fn start_vm(vm_id: VMId) -> AxResult {
        crate::runtime::start_vm(vm_id)
    }

    /// Stop a VM selected from the runtime registry.
    pub fn stop_vm(vm_id: VMId) -> AxResult {
        crate::runtime::stop_vm(vm_id)
    }

    /// Resume a VM selected from the runtime registry.
    pub fn resume_vm(vm_id: VMId) -> AxResult {
        crate::runtime::resume_vm(vm_id)
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

/// Set up the primary vCPU task for a prepared VM.
pub fn setup_primary_vcpu(vm: AxVMRef) {
    crate::runtime::setup_primary_vcpu(vm);
}

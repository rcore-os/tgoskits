//! AxVM runtime services backed by the default ArceOS host.

extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    vec::Vec,
};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::VMId;
use axvmconfig::{CpuPartitionPlan, VcpuTaskAffinity, VmCpuPartitionInput};

use crate::{
    AxVmError, AxVmResult,
    arch::ArchVCpu,
    ax_err,
    host::{HostPlatform, default_host},
    vcpu::get_current_vcpu,
    vm::AxVMRef,
};

/// AxVM runtime services.
///
/// The runtime owns host initialization and VM execution primitives. VM-set
/// orchestration belongs to the top-level hypervisor program.
pub struct AxvmRuntime {
    _private: (),
}

struct VmRegistry {
    vms: BTreeMap<VMId, AxVMRef>,
    cpu_partition_inputs: BTreeMap<VMId, VmCpuPartitionInput>,
    cpu_partition: CpuPartitionPlan,
    frozen_affinity_vms: BTreeSet<VMId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VcpuTaskPlacement {
    pub(crate) affinity: VcpuTaskAffinity,
    pub(crate) initial_cpu: usize,
}

impl VmRegistry {
    const fn new() -> Self {
        Self {
            vms: BTreeMap::new(),
            cpu_partition_inputs: BTreeMap::new(),
            cpu_partition: CpuPartitionPlan::empty(),
            frozen_affinity_vms: BTreeSet::new(),
        }
    }

    fn insert(&mut self, vm: AxVMRef, cpu_partition_input: VmCpuPartitionInput) -> AxVmResult {
        let vm_id = vm.id();
        if self.vms.contains_key(&vm_id) {
            return Err(AxVmError::resource_conflict(
                "VM registry",
                format!("VM[{vm_id}] is already registered"),
            ));
        }

        let mut candidate_inputs = self
            .cpu_partition_inputs
            .values()
            .cloned()
            .collect::<Vec<_>>();
        candidate_inputs.push(cpu_partition_input.clone());
        let candidate_partition = build_cpu_partition_plan(&candidate_inputs)?;
        self.ensure_frozen_affinities_unchanged(&candidate_partition)?;

        self.vms.insert(vm_id, vm);
        self.cpu_partition_inputs.insert(vm_id, cpu_partition_input);
        self.cpu_partition = candidate_partition;
        Ok(())
    }

    fn ensure_frozen_affinities_unchanged(
        &self,
        candidate_partition: &CpuPartitionPlan,
    ) -> AxVmResult {
        for vm_id in &self.frozen_affinity_vms {
            let vm = self
                .vms
                .get(vm_id)
                .expect("a frozen CPU affinity must belong to a registered VM");
            for vcpu_id in 0..vm.vcpu_num() {
                let current = task_placement(&self.cpu_partition, vm.id(), vcpu_id);
                let candidate = task_placement(candidate_partition, vm.id(), vcpu_id);
                if current != candidate {
                    return Err(AxVmError::resource_conflict(
                        "guest CPU partition",
                        format!(
                            "registering a VM would change frozen VM[{}] vCPU[{vcpu_id}] \
                             placement from {current:?} to {candidate:?}",
                            vm.id()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn rebuild_cpu_partition(&mut self) {
        let inputs = self
            .cpu_partition_inputs
            .values()
            .cloned()
            .collect::<Vec<_>>();
        self.cpu_partition = build_cpu_partition_plan(&inputs)
            .expect("removing a VM from a valid CPU partition must preserve validity");
    }
}

static VM_REGISTRY: Mutex<VmRegistry> = Mutex::new(VmRegistry::new());

fn vm_cpu_partition_input(vm: &AxVMRef) -> VmCpuPartitionInput {
    let affinities = vm
        .get_vcpu_affinities_pcpu_ids()
        .into_iter()
        .map(|(_, affinity, _)| affinity)
        .collect();
    VmCpuPartitionInput::new(vm.id(), vm.cpus_dedicated(), affinities)
}

fn build_cpu_partition_plan(inputs: &[VmCpuPartitionInput]) -> AxVmResult<CpuPartitionPlan> {
    CpuPartitionPlan::build(crate::percpu::enabled_cpu_mask(), inputs).map_err(Into::into)
}

/// Validate and register an externally initialized VM.
pub(crate) fn push_existing_vm(vm: AxVMRef) -> AxVmResult {
    let vm_id = vm.id();
    let cpu_partition_input = vm_cpu_partition_input(&vm);
    let mut registry = VM_REGISTRY.lock();
    if let Err(error) = registry.insert(vm, cpu_partition_input) {
        warn!("VM[{vm_id}] registration rejected: {error}");
        return Err(error);
    }
    Ok(())
}

/// Remove a VM from the process-wide AxVM runtime registry.
pub(crate) fn remove_existing_vm(vm_id: VMId) -> Option<AxVMRef> {
    crate::runtime::vcpus::cleanup_vm_vcpus(vm_id);
    let mut registry = VM_REGISTRY.lock();
    let removed = registry.vms.remove(&vm_id);
    if removed.is_some() {
        registry.cpu_partition_inputs.remove(&vm_id);
        registry.frozen_affinity_vms.remove(&vm_id);
        registry.rebuild_cpu_partition();
    }
    removed
}

/// Return a VM from the process-wide AxVM runtime registry.
pub fn get_vm_by_id(vm_id: VMId) -> Option<AxVMRef> {
    VM_REGISTRY.lock().vms.get(&vm_id).cloned()
}

/// Return all VMs known to the process-wide AxVM runtime registry.
pub fn get_vm_list() -> Vec<AxVMRef> {
    VM_REGISTRY.lock().vms.values().cloned().collect()
}

/// Run an operation with a VM selected from the process-wide runtime registry.
pub(crate) fn with_vm<F, R>(vm_id: VMId, f: F) -> Option<R>
where
    F: FnOnce(&AxVMRef) -> R,
{
    let vm = VM_REGISTRY.lock().vms.get(&vm_id).cloned();
    vm.map(|vm| f(&vm))
}

/// Return the validated affinity and initial host CPU for a registered vCPU.
pub(crate) fn vcpu_task_placement(vm_id: VMId, vcpu_id: usize) -> Option<VcpuTaskPlacement> {
    let mut registry = VM_REGISTRY.lock();
    let placement = task_placement(&registry.cpu_partition, vm_id, vcpu_id);
    if placement.is_some() {
        registry.frozen_affinity_vms.insert(vm_id);
    }
    placement
}

fn task_placement(
    plan: &CpuPartitionPlan,
    vm_id: VMId,
    vcpu_id: usize,
) -> Option<VcpuTaskPlacement> {
    Some(VcpuTaskPlacement {
        affinity: plan.task_affinity(vm_id, vcpu_id)?,
        initial_cpu: plan.task_initial_cpu(vm_id, vcpu_id)?,
    })
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

/// Inject a virtual interrupt into a VM's vCPU.
#[expect(
    dead_code,
    reason = "only the LoongArch IRQ backend injects external VM interrupts"
)]
pub(crate) fn inject_vm_vcpu_interrupt(vm_id: VMId, vcpu_id: usize, vector: usize) -> AxVmResult {
    use crate::AsVCpuTask;

    let current = crate::host::task::current_task();
    if let Some(task) = current.try_as_vcpu_task()
        && task.vm().id() == vm_id
        && task.vcpu.id() == vcpu_id
    {
        return task.vcpu.inject_interrupt(vector);
    }

    crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector)
}

/// Return the current VM ID from the vCPU currently executing on this CPU.
pub fn current_vm_id() -> Option<VMId> {
    get_current_vcpu::<ArchVCpu>().map(|vcpu| vcpu.vm_id())
}

/// Return the current vCPU ID from the vCPU currently executing on this CPU.
pub fn current_vcpu_id() -> Option<usize> {
    get_current_vcpu::<ArchVCpu>().map(|vcpu| vcpu.id())
}

/// Inject a virtual interrupt into the vCPU currently executing on this CPU.
pub fn inject_current_vcpu_interrupt(vector: usize) -> AxVmResult {
    let vcpu = get_current_vcpu::<ArchVCpu>().ok_or_else(|| {
        AxVmError::resource_unavailable("current vCPU", "current vCPU is not set")
    })?;
    vcpu.inject_interrupt(vector)
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

/// Validate and register a prepared VM in the AxVM runtime.
pub fn try_register_vm(vm: AxVMRef) -> AxVmResult {
    crate::runtime::try_register_vm(vm)
}

/// Register a prepared VM, returning `false` when validation or insertion fails.
pub fn register_vm(vm: AxVMRef) -> bool {
    try_register_vm(vm).is_ok()
}

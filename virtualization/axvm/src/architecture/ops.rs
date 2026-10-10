//! Hardware-only vCPU boundary selected once for the current architecture.

use std::{sync::Arc, vec::Vec};

use axaddrspace::NestedPageTableOps;
use axvm_types::{VmArchPerCpuOps, VmArchVcpuOps};

use crate::{
    AxVmResult,
    engine::VcpuAction,
    irq::model::PendingVcpuInterrupt,
    runtime::{QueuedVcpuInterrupt, hvc::GuestRequest},
    services::{RunServices, RunSignals, VcpuWait},
    vm::{AxVM, AxVMResources},
};

/// Portable register effects produced after hardware has been unloaded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RegisterCompletion {
    #[default]
    None,
    Gpr {
        register: usize,
        value: usize,
    },
    Return(usize),
}

/// Each entry value retains only prepared hardware and interrupt capabilities.
/// Device callbacks and task-service locks belong in `RunServices` instead.
pub(crate) trait ArchOps {
    /// Owner-local architecture backend. All mutable state is accessed through
    /// `&mut`; scheduling, locking, and lifecycle ownership remain in AxVM.
    type VCpu: VmArchVcpuOps + Send;
    type PerCpu: VmArchPerCpuOps;
    type NestedPageTable: NestedPageTableOps;
    type Entry: Send + Sync;
    type Exit: Send;
    type Completion: Send + Default + From<RegisterCompletion>;

    fn has_hardware_support() -> bool;

    /// Retires translations for the old root on each CPU that could cache them.
    /// Called in a synchronous host CPU rendezvous after every vCPU is unloaded.
    fn invalidate_translations(
        entry: &Self::Entry,
        old_root: axvm_types::NestedPagingConfig,
    ) -> AxVmResult;
    fn prepare_entry(
        resources: &AxVMResources,
        signals: Arc<RunSignals>,
    ) -> AxVmResult<Self::Entry>;

    /// These lifecycle hooks run on the unique control owner in task context.
    fn enter_runtime(vm: &mut AxVM, signals: &Arc<RunSignals>) -> AxVmResult;
    fn exit_runtime(vm: &mut AxVM, signals: &Arc<RunSignals>) -> AxVmResult;

    /// All task-side preparation finishes before CPU binding and IRQ masking.
    fn prepare_vcpu(vcpu: &mut Self::VCpu, entry: &Self::Entry) -> AxVmResult;
    /// Quiesces per-vCPU task producers while preserving their guest state.
    fn suspend_vcpu(_vcpu: &mut Self::VCpu) -> AxVmResult {
        Ok(())
    }

    /// Restarts quiesced per-vCPU producers before opening guest admission.
    fn resume_vcpu(_vcpu: &mut Self::VCpu) -> AxVmResult {
        Ok(())
    }

    /// Stops task-side vCPU producers before returning or releasing a backend.
    /// Architectures without a per-vCPU producer need no retirement work.
    fn quiet_vcpu(_vcpu: &mut Self::VCpu) -> AxVmResult {
        Ok(())
    }

    /// Rechecks CPU-local resource ownership after task preparation and pinning.
    /// False retires this attempt without guest entry; preparation may then
    /// perform a remote handoff in task context before the next pinned attempt.
    /// Backends without retained CPU-local claims are immediately ready.
    fn entry_cpu_is_ready(_vcpu: &mut Self::VCpu) -> bool {
        true
    }

    fn before_guest(vcpu: &mut Self::VCpu, vcpu_id: usize, entry: &Self::Entry) -> AxVmResult;
    fn complete(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        completion: Self::Completion,
    ) -> AxVmResult;
    fn capture_exit(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<Self::Exit>;
    /// Resolves software-only exits after unloading and restoring the CPU.
    /// Already durable exits need no further backend work. An architecture
    /// that defers CSR or timer emulation overrides this task-context stage.
    fn finish_exit(
        _vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        exit: Self::Exit,
    ) -> AxVmResult<Self::Exit> {
        Ok(exit)
    }
    fn handle_exit(
        exit: Self::Exit,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<Self::Completion, GuestRequest>>;

    /// Arm/PLIC/LAPIC native pending and source state remains authoritative.
    fn inject_vcpu_interrupt(vcpu: &mut Self::VCpu, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("inject vCPU interrupt", error))
    }
    fn inject_arch_interrupt(
        vcpu: &mut Self::VCpu,
        vcpu_id: usize,
        entry: &Self::Entry,
        interrupt: QueuedVcpuInterrupt,
    ) -> AxVmResult;

    /// Invoked with an unloaded backend; wait predicates use only lower state.
    fn wait_for_event(
        vcpu: &mut Self::VCpu,
        vcpu_id: usize,
        entry: &Self::Entry,
        wait: &VcpuWait,
    ) -> AxVmResult;
}

/// Real CPU_ON capability provided only by Arm and RISC-V.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) trait CpuOn: ArchOps {
    fn initialize_cpu_on(
        vcpu: &mut Self::VCpu,
        entry: axvm_types::GuestPhysAddr,
        argument: usize,
    ) -> AxVmResult;
}

pub(crate) fn target_phys_cpu_ids(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> Vec<usize> {
    let mut cpu_ids = Vec::new();
    for (_, maybe_mask, phys_id) in vcpu_mappings {
        if let Some(mask) = maybe_mask {
            for cpu_id in 0..usize::BITS as usize {
                if mask & (1usize << cpu_id) != 0 && !cpu_ids.contains(&cpu_id) {
                    cpu_ids.push(cpu_id);
                }
            }
        } else if !cpu_ids.contains(phys_id) {
            cpu_ids.push(*phys_id);
        }
    }
    cpu_ids
}

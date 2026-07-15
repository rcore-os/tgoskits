//! Core vCPU and nested-paging contract implemented by every target architecture.

use alloc::vec::Vec;

use ax_kspin::PreemptGuard;
use ax_memory_addr::VirtAddr;
use axaddrspace::NestedPageTableOps;
use axvm_types::{VmArchPerCpuOps, VmArchVcpuOps};

use super::{BoundVcpuExit, CommonDeferredRunWork, VcpuRunAction};
use crate::{
    AxVmResult,
    vcpu::{BoundVcpu, PinnedCpuContext},
};

pub(crate) trait ArchOps {
    type VCpu: VmArchVcpuOps;
    type PerCpu: VmArchPerCpuOps;
    type DeferredRunWork: Copy + 'static + From<CommonDeferredRunWork>;
    type NestedPageTable: NestedPageTableOps;

    fn has_hardware_support() -> bool;

    fn clean_dcache_range(_addr: VirtAddr, _size: usize) {}

    fn register_platform_irq_injector() {}

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets)
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn before_first_run(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    fn before_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &BoundVcpu<'_, '_, Self::VCpu>) {}

    fn inject_pending_interrupt(
        _vm: &crate::AxVMRef,
        vcpu: &BoundVcpu<'_, '_, Self::VCpu>,
        interrupt: crate::vm::PendingInterrupt,
    ) -> AxVmResult {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                vcpu.inject_interrupt(vector)
            }
            crate::vm::PendingInterrupt::Triggered { vector, trigger } => {
                trace!(
                    "Injecting queued {trigger:?} interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                vcpu.inject_interrupt_with_trigger(vector, trigger)
            }
            crate::vm::PendingInterrupt::External {
                vector,
                physical_irq,
            } => {
                warn!(
                    "VM[{}] VCpu[{}] dropped unsupported external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x}",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                Ok(())
            }
        }
    }

    fn on_last_vcpu_exit(_vm_id: usize) {}

    fn after_mmio_read(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    fn after_mmio_write(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    /// Handles work that is safe while the vCPU remains bound to one host CPU.
    ///
    /// Preemption is disabled and `CURRENT_VCPU` is published for this call.
    /// Implementations must not block, yield, or retain CPU-local references;
    /// return the architecture's deferred-work result for operations that
    /// require a normal task context.
    fn handle_vcpu_exit_bound<'cpu>(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit<'cpu>,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self::VCpu: 'cpu;

    /// Finishes exit work after backend unbind and CPU-local cleanup.
    ///
    /// This hook runs with normal host preemption restored and may perform
    /// blocking or scheduler-facing operations.
    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction>;

    fn run_vcpu(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult<VcpuRunAction>
    where
        Self: Sized,
    {
        match run_vcpu_pinned::<Self>(vm, vcpu)? {
            BoundVcpuExit::Complete(action) => Ok(action),
            BoundVcpuExit::Defer(work) => Self::finish_deferred_run_work(vm, vcpu, work),
            BoundVcpuExit::Continue => unreachable!("continued exits do not leave run loop"),
        }
    }
}

fn run_vcpu_pinned<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
    let preempt_guard = PreemptGuard::new();
    let pinned_cpu = PinnedCpuContext::new(preempt_guard.cpu_pin());
    let _current_vcpu = vcpu.enter_pinned(&pinned_cpu);

    // Every run acquires a fresh CPU binding. A previous `Ready` state can no
    // longer be resumed on an unverified CPU.
    vcpu.bind(&pinned_cpu)?;
    let run_result = {
        let bound_vcpu = BoundVcpu::new(vcpu, &pinned_cpu);
        A::before_vcpu_run(vm, &bound_vcpu);

        loop {
            if let Err(error) =
                crate::runtime::vcpus::inject_pending_interrupts::<A>(vm, &bound_vcpu)
            {
                break Err(error);
            }
            if let Err(error) = bound_vcpu.drain_published_interrupts() {
                break Err(error);
            }

            let exit = match vcpu.run(&pinned_cpu) {
                Ok(exit) => exit,
                Err(error) => break Err(error),
            };
            let exit_result = A::handle_vcpu_exit_bound(vm, vcpu, exit);
            trace!("VM[{}] VCpu[{}] completed a bound exit", vm.id(), vcpu.id());
            match exit_result {
                Ok(BoundVcpuExit::Continue) => {}
                Ok(action) => break Ok(action),
                Err(error) => break Err(error),
            }
        }
    };

    // Backend unbind restores host-owned CPU state before CURRENT_VCPU is
    // cleared by `_current_vcpu` and preemption is restored by `preempt_guard`.
    if let Err(error) = vcpu.unbind(&pinned_cpu) {
        panic!(
            "fatal vCPU cleanup invariant: VM[{}] VCpu[{}] could not restore host state: {error:?}",
            vm.id(),
            vcpu.id()
        );
    }
    run_result
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

pub(crate) fn default_vcpu_affinities(
    cpu_num: usize,
    phys_cpu_ids: Option<&[usize]>,
    phys_cpu_sets: Option<&[usize]>,
) -> Vec<(usize, Option<usize>, usize)> {
    let mut vcpus = Vec::with_capacity(cpu_num);
    for vcpu_id in 0..cpu_num {
        vcpus.push((vcpu_id, None, vcpu_id));
    }

    if let Some(phys_cpu_sets) = phys_cpu_sets {
        for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.1 = Some(*pcpu_mask_bitmap);
            }
        }
    }

    if let Some(phys_cpu_ids) = phys_cpu_ids {
        for (vcpu_id, phys_id) in phys_cpu_ids.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.2 = *phys_id;
            }
        }
    }

    vcpus
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_runner_always_rebinds_and_treats_cleanup_failure_as_fatal() {
        let architecture_ops = include_str!("ops.rs");
        let pinned_runner = architecture_ops
            .split_once("fn run_vcpu_pinned")
            .expect("AxVM must have one pinned backend runner")
            .1
            .split_once("pub(crate) fn target_phys_cpu_ids")
            .expect("pinned runner must end before affinity helpers")
            .0;

        assert!(pinned_runner.contains("vcpu.bind(&pinned_cpu)?"));
        assert!(pinned_runner.contains("if let Err(error) = vcpu.unbind(&pinned_cpu)"));
        assert!(pinned_runner.contains("fatal vCPU cleanup invariant"));
        assert!(
            !pinned_runner.contains("VmVcpuState::Ready"),
            "a stale bound state must never bypass a fresh CPU bind"
        );
    }
}

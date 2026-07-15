//! Core vCPU and nested-paging contract implemented by every target architecture.

use alloc::{format, vec::Vec};

use ax_memory_addr::VirtAddr;
use axaddrspace::NestedPageTableOps;
use axvm_types::{VmArchPerCpuOps, VmArchVcpuOps, VmVcpuState};

use super::{BoundVcpuExit, VcpuRunAction};
use crate::{AxVmResult, ax_err};

pub(crate) trait ArchOps {
    type VCpu: VmArchVcpuOps;
    type PerCpu: VmArchPerCpuOps;
    type DeferredRunWork;
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

    fn before_first_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

    fn before_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

    fn deliver_pending_controller_interrupts(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) {
    }

    fn after_external_interrupt(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: usize,
    ) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
    }

    fn on_last_vcpu_exit(_vm_id: usize) {}

    fn after_mmio_write(_vm: &crate::AxVMRef) {}

    fn with_vcpu_interrupt_context<T>(_vm: &crate::AxVMRef, run: impl FnOnce() -> T) -> T {
        run()
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>;

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
        let vm_id = vm.id();
        let vcpu_id = vcpu.id();
        let interrupt_vcpu = axdevice::VcpuInterruptId::new(vcpu_id);
        let interrupt_topology = vm.prepared_interrupt_topology()?;

        match vcpu.state() {
            VmVcpuState::Free => vcpu.bind()?,
            VmVcpuState::Ready => {}
            state => {
                return ax_err!(
                    BadState,
                    format!("VCpu state is not Free or Ready, but {state:?}")
                );
            }
        }

        let run_result = vcpu.with_current_cpu_set(|| {
            Self::with_vcpu_interrupt_context(vm, || -> AxVmResult<_> {
                interrupt_topology.load_vcpu(interrupt_vcpu)?;
                let result = (|| {
                    loop {
                        Self::deliver_pending_controller_interrupts(vm, vcpu);
                        interrupt_topology.synchronize_vcpu(interrupt_vcpu)?;

                        let exit = vcpu.run()?;
                        trace!("{exit:#x?}");
                        // Port/MMIO writes and EOIs can change controller state. Apply those exit
                        // side effects before making queued controller inputs deliverable.
                        let action = Self::handle_vcpu_exit_bound(vm, vcpu, exit)?;
                        interrupt_topology.synchronize_vcpu(interrupt_vcpu)?;
                        match action {
                            BoundVcpuExit::Continue => continue,
                            action => break Ok(action),
                        }
                    }
                })();
                let save_result = interrupt_topology.save_vcpu(interrupt_vcpu);
                match result {
                    Ok(action) => {
                        save_result?;
                        Ok(action)
                    }
                    Err(error) => {
                        if let Err(save_error) = save_result {
                            warn!(
                                "VM[{vm_id}] VCpu[{vcpu_id}] interrupt-controller save after run \
                                 error failed: {save_error}"
                            );
                        }
                        Err(error)
                    }
                }
            })
        });

        let unbind_result = vcpu.unbind();
        match run_result {
            Ok(BoundVcpuExit::Complete(action)) => {
                unbind_result?;
                Ok(action)
            }
            Ok(BoundVcpuExit::Defer(work)) => {
                unbind_result?;
                Self::finish_deferred_run_work(vm, vcpu, work)
            }
            Ok(BoundVcpuExit::Continue) => unreachable!("continued exits do not leave run loop"),
            Err(err) => {
                if let Err(unbind_err) = unbind_result {
                    warn!(
                        "VM[{vm_id}] VCpu[{vcpu_id}] unbind after run error failed: {unbind_err:?}"
                    );
                }
                Err(err)
            }
        }
    }
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

use std::os::arceos::modules::{ax_hal, ax_task};

use ax_errno::{AxResult, ax_err_type};
use axaddrspace::{HostPhysAddr, HostVirtAddr};
use axvisor_api::vmm::{InterruptVector, VCpuId, VCpuSet, VMId, VmmIf};

use crate::{task::AsVCpuTask, vmm};

struct VmmImpl;

fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
    ax_hal::mem::virt_to_phys(vaddr)
}

fn current_time_nanos() -> u64 {
    ax_hal::time::monotonic_time_nanos()
}

fn current_vm_id() -> usize {
    ax_task::current().as_vcpu_task().vm().id()
}

fn current_vcpu_id() -> usize {
    ax_task::current().as_vcpu_task().vcpu.id()
}

fn current_pcpu_id() -> usize {
    ax_hal::percpu::this_cpu_id()
}

fn vcpu_resides_on(vm_id: usize, vcpu_id: usize) -> AxResult<usize> {
    vmm::with_vcpu_task(vm_id, vcpu_id, |task| task.cpu_id() as usize)
        .ok_or_else(|| ax_err_type!(NotFound))
}

fn inject_irq_to_vcpu(vm_id: usize, vcpu_id: usize, irq: usize) -> AxResult {
    vmm::with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
        vcpu.inject_interrupt(irq).unwrap();
    })
}

#[axvisor_api::api_impl]
impl VmmIf for VmmImpl {
    fn current_vm_id() -> usize {
        ax_task::current().as_vcpu_task().vm().id()
    }

    fn current_vcpu_id() -> usize {
        ax_task::current().as_vcpu_task().vcpu.id()
    }

    fn vcpu_num(vm_id: VMId) -> Option<usize> {
        vmm::with_vm(vm_id, |vm| vm.vcpu_num())
    }

    fn active_vcpus(_vm_id: VMId) -> Option<usize> {
        todo!("active_vcpus")
    }

    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) {
        let _ = vmm::with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
            vcpu.inject_interrupt(vector as usize).unwrap();
        });
    }

    fn inject_interrupt_to_cpus(_vm_id: VMId, _vcpu_set: VCpuSet, _vector: InterruptVector) {
        todo!("inject_interrupt_to_cpus")
    }

    fn notify_vcpu_timer_expired(_vm_id: VMId, _vcpu_id: VCpuId) {
        todo!("notify_vcpu_timer_expired")
        // vmm::timer::notify_timer_expired(vm_id, vcpu_id);
    }
}

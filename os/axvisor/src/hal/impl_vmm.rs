use std::os::arceos::modules::ax_task;

use axvisor_api::vmm::{InterruptVector, VCpuId, VCpuSet, VMId, VmmIf};

use crate::{task::AsVCpuTask, vmm};

struct VmmImpl;

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

    fn active_vcpus(vm_id: VMId) -> Option<usize> {
        vmm::with_vm(vm_id, |vm| {
            (0..vm.vcpu_num())
                .filter(|&id| vm.vcpu_pcpu(id).is_some())
                .count()
        })
    }

    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) {
        let _ = vmm::with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
            vcpu.inject_interrupt(vector as usize).unwrap();
        });
    }

    fn inject_interrupt_to_cpus(vm_id: VMId, vcpu_set: VCpuSet, vector: InterruptVector) {
        if let Some(vcpu_num) = Self::vcpu_num(vm_id) {
            for vcpu_id in 0..vcpu_num {
                if vcpu_set.get(vcpu_id) {
                    let _ = vmm::with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
                        vcpu.inject_interrupt(vector as usize).unwrap();
                    });
                }
            }
        }
    }

    fn notify_vcpu_timer_expired(_vm_id: VMId, _vcpu_id: VCpuId) {
        todo!("notify_vcpu_timer_expired")
        // vmm::timer::notify_timer_expired(vm_id, vcpu_id);
    }
}

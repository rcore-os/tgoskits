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
            let vcpu_num = vm.vcpu_num();
            if vcpu_num >= usize::BITS as usize {
                usize::MAX
            } else {
                // The VmmIf contract returns an active-vCPU bitmask, not a count.
                (1usize << vcpu_num) - 1
            }
        })
    }

    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) {
        let _ = vmm::with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
            vcpu.inject_interrupt(vector as usize).unwrap();
        });
    }

    fn inject_interrupt_to_cpus(vm_id: VMId, vcpu_set: VCpuSet, vector: InterruptVector) {
        for vcpu_id in &vcpu_set {
            Self::inject_interrupt(vm_id, vcpu_id, vector);
        }
    }

    fn notify_vcpu_timer_expired(_vm_id: VMId, _vcpu_id: VCpuId) {
        todo!("notify_vcpu_timer_expired")
        // vmm::timer::notify_timer_expired(vm_id, vcpu_id);
    }
}

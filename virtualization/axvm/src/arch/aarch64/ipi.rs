//! AArch64 guest IPI target resolution and injection.

use crate::{AxVmResult, architecture::VcpuRunAction};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SendIpiExit {
    pub(crate) target_cpu: u64,
    pub(crate) target_cpu_aux: u64,
    pub(crate) send_to_all: bool,
    pub(crate) send_to_self: bool,
    pub(crate) vector: u64,
}

pub(crate) fn finish(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    exit: SendIpiExit,
) -> AxVmResult<VcpuRunAction> {
    let vm_id = vm.id();
    debug!(
        "VM[{vm_id}] run VCpu[{vcpu_id}] SendIPI, target_cpu={:#x}, target_cpu_aux={:#x}, \
         vector={}",
        exit.target_cpu, exit.target_cpu_aux, exit.vector
    );
    let targets = ipi_targets(vm, vcpu_id, exit);
    if targets.is_empty() {
        warn!(
            "VM[{vm_id}] SendIPI has no target: target_cpu={:#x}, target_cpu_aux={:#x}",
            exit.target_cpu, exit.target_cpu_aux
        );
        return Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        });
    }

    // Deferred work runs after CURRENT_VCPU has been unpublished. Queue every
    // target uniformly so a self-directed SGI is delivered on the next bound
    // entry instead of relying on a stale current-vCPU publication.
    if let Err(err) = vm.inject_interrupt_to_vcpu(targets, exit.vector as _) {
        warn!(
            "Failed to inject interrupt {} to VM[{vm_id}] targets {targets:?}: {err:?}",
            exit.vector
        );
    }
    Ok(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    })
}

fn ipi_targets(
    vm: &crate::AxVMRef,
    current_vcpu_id: usize,
    exit: SendIpiExit,
) -> crate::CpuMask<64> {
    let mut targets = crate::CpuMask::new();
    if exit.send_to_all {
        for vcpu in vm.vcpu_list() {
            if vcpu.id() != current_vcpu_id {
                targets.set(vcpu.id(), true);
            }
        }
    } else if exit.send_to_self {
        targets.set(current_vcpu_id, true);
    } else {
        for (vcpu_id, _, phys_id) in vm.get_vcpu_affinities_pcpu_ids() {
            let affinity = phys_id as u64;
            let aff0 = affinity & 0xff;
            let aff123 = affinity & !0xff;
            if aff123 == exit.target_cpu && aff0 < 16 && (exit.target_cpu_aux & (1u64 << aff0)) != 0
            {
                targets.set(vcpu_id, true);
            }
        }
    }
    targets
}

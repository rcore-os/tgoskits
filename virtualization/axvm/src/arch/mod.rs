//! Architecture component glue owned by AxVM.

use alloc::vec::Vec;

use ax_errno::AxResult;
use ax_memory_addr::VirtAddr;
use axvm_types::{
    GuestPhysAddr, PassThroughPortConfig, VMInterruptMode, VmArchPerCpuOps, VmArchVcpuOps,
};

use crate::CpuMask;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) type CurrentArch = aarch64::Aarch64Arch;
#[cfg(target_arch = "loongarch64")]
pub(crate) type CurrentArch = loongarch64::LoongArch64Arch;
#[cfg(target_arch = "riscv64")]
pub(crate) type CurrentArch = riscv64::Riscv64Arch;
#[cfg(target_arch = "x86_64")]
pub(crate) type CurrentArch = x86_64::X86_64Arch;

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;

#[allow(dead_code)]
pub(crate) struct VcpuCreateContext {
    pub(crate) vcpu_id: usize,
    pub(crate) phys_cpu_id: usize,
    pub(crate) dtb_addr: Option<GuestPhysAddr>,
    pub(crate) firmware_boot: bool,
}

#[allow(dead_code)]
pub(crate) struct VcpuSetupContext<'a> {
    pub(crate) interrupt_mode: VMInterruptMode,
    pub(crate) emulates_console: bool,
    pub(crate) passthrough_ports: &'a [PassThroughPortConfig],
    pub(crate) firmware_boot: bool,
}

pub(crate) trait ArchOps {
    type VCpu: VmArchVcpuOps;
    type PerCpu: VmArchPerCpuOps;
    type VcpuCreateState;

    fn has_hardware_support() -> bool;

    fn max_guest_page_table_levels() -> usize {
        4
    }

    fn clean_dcache_range(_addr: VirtAddr, _size: usize) {}

    fn new_vcpu_create_state(
        vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState>;

    fn build_vcpu_create_config(
        state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::CreateConfig>;

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig>;

    fn register_platform_irq_injector() {}

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets)
    }

    fn ipi_targets(
        vm: &crate::AxVMRef,
        current_vcpu_id: usize,
        target_cpu: u64,
        target_cpu_aux: u64,
        send_to_all: bool,
        send_to_self: bool,
    ) -> CpuMask<64> {
        let mut targets = CpuMask::new();

        if send_to_all {
            for vcpu in vm.vcpu_list() {
                if vcpu.id() != current_vcpu_id {
                    targets.set(vcpu.id(), true);
                }
            }
        } else if send_to_self {
            targets.set(current_vcpu_id, true);
        } else {
            let _ = target_cpu_aux;
            targets.set(target_cpu as usize, true);
        }

        targets
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef) {
        vcpu.set_gpr(0, 0);
    }

    fn set_io_read_result(vcpu: &crate::vm::AxVCpuRef, val: usize) {
        vcpu.set_gpr(0, val);
    }

    fn before_first_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {}

    fn before_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {}

    fn inject_pending_interrupt(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
        interrupt: crate::vm::PendingInterrupt,
    ) {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                if let Err(err) = vcpu.inject_interrupt(vector) {
                    warn!(
                        "Failed to inject queued interrupt {vector:#x} into VM[{}] VCpu[{}]: \
                         {err:?}",
                        vcpu.vm_id(),
                        vcpu.id()
                    );
                }
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
            }
        }
    }

    fn after_external_interrupt(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef, vector: usize) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
    }

    fn after_preemption_timer(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {
        crate::check_timer_events();
    }

    fn after_interrupt_end(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef,
        _vector: Option<u8>,
    ) {
    }

    fn handle_halt(runtime: &crate::vm::VmRuntimeHandle) -> bool {
        runtime.wait();
        false
    }

    fn handle_idle(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {
        crate::check_timer_events();
    }

    fn on_last_vcpu_exit(_vm_id: usize) {}

    fn after_mmio_write(_vm: &crate::AxVM) {}
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

use alloc::vec::Vec;

use ax_crate_interface::impl_interface;
use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};
use riscv_vcpu::{GprIndex as RiscvGprIndex, host::RiscvVcpuHostIf};
use riscv_vplic::host::RiscvVplicHostIf;

use super::{ArchOps, VcpuCreateContext, VcpuSetupContext, default_vcpu_affinities};
use crate::host::{HostMemory, default_host};

pub(crate) struct Riscv64Arch;

impl ArchOps for Riscv64Arch {
    type VCpu = riscv_vcpu::RISCVVCpu;
    type PerCpu = riscv_vcpu::RISCVPerCpu;
    type VcpuCreateState = ();

    fn has_hardware_support() -> bool {
        riscv_vcpu::has_hardware_support()
    }

    fn new_vcpu_create_state(
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState> {
        Ok(())
    }

    fn build_vcpu_create_config(
        _state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::CreateConfig> {
        Ok(riscv_vcpu::RISCVVCpuCreateConfig {
            hart_id: ctx.vcpu_id,
            dtb_addr: ctx.dtb_addr.unwrap_or_default().as_usize(),
        })
    }

    fn build_vcpu_setup_config(
        _ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::SetupConfig> {
        Ok(Default::default())
    }

    fn register_platform_irq_injector() {
        register_platform_irq_injector();
    }

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        let mut vcpus = default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets);
        if phys_cpu_sets.is_none() {
            for (_, mask, phys_id) in &mut vcpus {
                *mask = Some(1 << *phys_id);
            }
        }
        vcpus
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef, vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(RiscvGprIndex::A1 as usize, arg);
    }

    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, 0);
    }

    fn set_io_read_result(vcpu: &crate::vm::AxVCpuRef, val: usize) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, val);
    }

    fn after_external_interrupt(_vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef, vector: usize) {
        vcpu.with_current_cpu_set(|| {
            crate::host::arceos::dispatch_host_irq(vector);
            vcpu.get_arch_vcpu().latch_hvip_from_hw();
        });
        crate::check_timer_events();
    }
}

struct RiscvVcpuHostIfImpl;

#[impl_interface]
impl RiscvVcpuHostIf for RiscvVcpuHostIfImpl {
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        default_host().virt_to_phys(vaddr)
    }
}

struct RiscvVplicHostIfImpl;

#[impl_interface]
impl RiscvVplicHostIf for RiscvVplicHostIfImpl {
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }
}

fn register_platform_irq_injector() {
    axplat_dyn::register_virtual_irq_injector(inject_virtual_irq);
}

fn inject_virtual_irq(irq_id: usize) -> bool {
    debug!("injecting RISC-V virtual IRQ id: {irq_id}");

    let Some(vm_id) = crate::current_vm_id() else {
        warn!("cannot inject RISC-V virtual IRQ without current VM context");
        return false;
    };

    let Some(injected) = crate::manager::with_vm(vm_id, |vm| {
        if let Err(err) = vm.pulse_interrupt(irq_id) {
            warn!("failed to inject RISC-V virtual IRQ {irq_id}: {err:?}");
            return false;
        }
        true
    }) else {
        warn!("cannot inject RISC-V virtual IRQ {irq_id}: VM[{vm_id}] not found");
        return false;
    };

    injected
}

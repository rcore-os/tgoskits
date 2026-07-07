use alloc::vec::Vec;

use ax_crate_interface::impl_interface;
use ax_errno::{AxResult, ax_err};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{NestedPagingConfig, VMInterruptMode};
use riscv_vcpu::{GprIndex as RiscvGprIndex, host::RiscvVcpuHostIf};
use riscv_vplic::host::RiscvVplicHostIf;

use super::{
    ArchOps, VcpuCreateContext, VcpuSetupContext, default_vcpu_affinities, target_phys_cpu_ids,
};
use crate::host::{HostMemory, default_host};

mod npt;

pub(crate) struct Riscv64Arch;

impl ArchOps for Riscv64Arch {
    type VCpu = riscv_vcpu::RISCVVCpu;
    type PerCpu = riscv_vcpu::RISCVPerCpu;
    type VcpuCreateState = ();
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        riscv_vcpu::has_hardware_support()
    }

    fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxResult<usize> {
        let mut levels = riscv_vcpu::max_guest_page_table_levels();
        for cpu_id in target_phys_cpu_ids(vcpu_mappings) {
            levels = levels.min(
                crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
                    .unwrap_or_else(riscv_vcpu::max_guest_page_table_levels),
            );
        }
        match levels {
            3 | 4 => Ok(levels),
            _ => ax_err!(Unsupported, "no supported RISC-V G-stage paging mode"),
        }
    }

    fn nested_paging_config(
        root_paddr: PhysAddr,
        levels: usize,
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<NestedPagingConfig> {
        match levels {
            3 => Ok(NestedPagingConfig::new(root_paddr, 3, 41, 8)),
            4 => Ok(NestedPagingConfig::new(root_paddr, 4, 50, 9)),
            _ => ax_err!(InvalidInput, "unsupported RISC-V G-stage levels"),
        }
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
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

    fn before_first_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef) {
        if vm.interrupt_mode() != VMInterruptMode::Passthrough {
            return;
        }
        let Some(cpu_id) = vcpu.phys_cpu_set().and_then(first_cpu_in_mask) else {
            warn!(
                "skip RISC-V virtual IRQ affinity for VM[{}] VCpu[{}]: no fixed host CPU",
                vm.id(),
                vcpu.id()
            );
            return;
        };
        let irq_sources = vm.with_config(|config| config.pass_through_irqs().to_vec());
        crate::irq::set_riscv_virtual_irq_targets(cpu_id, &irq_sources);
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
    crate::irq::register_riscv_virtual_irq_injector(inject_virtual_irq);
}

fn first_cpu_in_mask(mask: usize) -> Option<usize> {
    (mask != 0).then_some(mask.trailing_zeros() as usize)
}

fn inject_virtual_irq(irq_id: usize) -> bool {
    let Some(vm_id) = crate::current_vm_id() else {
        trace!("skip RISC-V virtual IRQ {irq_id}: no current VM context");
        return false;
    };

    debug!("injecting RISC-V virtual IRQ id: {irq_id}");

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
